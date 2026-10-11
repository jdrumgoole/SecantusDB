"""Differential-probe the cursor commands against a real mongod.

`find` and what its options do to the first batch and the cursor, `getMore`,
`killCursors`, the cursors `aggregate` and the listing commands open,
tailable cursors on a capped collection, and `count` and `distinct`, which
read without one. Commands go over a raw connection with no session, so a
cursor id can be handed to the next command as it is.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/cursor_commands.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Decimal128, Int64  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "localTime")


class Cur:
    """The id of a cursor an earlier step saved under `label`."""

    def __init__(self, label="k"):
        self.label = label

    def __repr__(self):
        return f"<cursor {self.label}>"


def find(filter_=None, **more):
    return {"find": "c", "filter": filter_ or {}, **more}


def more(**kw):
    return {"getMore": Cur(), "collection": "c", **kw}


def agg(*pipeline, **kw):
    return {"aggregate": "c", "pipeline": list(pipeline), "cursor": {}, **kw}


SORTED = {"sort": {"_id": 1}}
DOCS = [
    {"_id": 1, "v": 5, "t": ["a", "b"]},
    {"_id": 2, "v": 3, "t": ["b"]},
    {"_id": 3, "v": 5, "t": []},
    {"_id": 4, "v": "x", "n": {"k": 1}},
    {"_id": 5, "v": None},
    {"_id": 6},
    {"_id": 7, "v": 2.5, "n": {"k": [1, 2]}},
    {"_id": 8, "v": [3, 9], "n": [{"k": 4}, {"k": 5}]},
]
SEED = [
    {"insert": "c", "documents": DOCS},
    {"createIndexes": "c", "indexes": [{"key": {"v": 1}, "name": "v_1"}]},
]

S: list[tuple[str, list]] = []


def scenario(name, *steps, seed=True):
    S.append((name, (SEED if seed else []) + list(steps)))


def each(name, commands):
    """One scenario of independent commands over the seeded collection."""
    scenario(name, *commands)


BAD = ["x", 2.5, -1, None, True, {}, [1], Int64(2), 2.0, Decimal128("2"), 0]

# ------------------------------------------------------------------- find

each("find: batchSize values", [find(batchSize=v, **SORTED) for v in BAD + [1, 3, 100]])
each("find: limit values", [find(limit=v, **SORTED) for v in BAD + [1, 3, 100, -3]])
each("find: skip values", [find(skip=v, **SORTED) for v in BAD + [1, 3, 100]])
each(
    "find: singleBatch values",
    [find(singleBatch=v, batchSize=2, **SORTED) for v in ["x", 1, 0, None, True, False, {}]],
)
each(
    "find: limit against batchSize",
    [
        find(limit=2, batchSize=3, **SORTED),
        find(limit=3, batchSize=3, **SORTED),
        find(limit=4, batchSize=3, **SORTED),
        find(limit=-2, batchSize=3, **SORTED),
        find(limit=-4, batchSize=3, **SORTED),
        find(limit=3, singleBatch=True, **SORTED),
        find(limit=3, skip=7, **SORTED),
        find(limit=3, skip=8, **SORTED),
        find(batchSize=8, **SORTED),
        find(batchSize=9, **SORTED),
        find({"_id": {"$lte": 3}}, batchSize=3),
        find({"_id": 99}, batchSize=0),
    ],
)
each(
    "find: the filter, sort and projection arguments",
    [{"find": "c", "filter": v} for v in [1, "x", None, [], True]]
    + [
        {"find": "c", "sort": v}
        for v in [
            1,
            "x",
            None,
            [],
            {"v": 2},
            {"v": "x"},
            {"v": 0},
            {},
            {"v": 1.0},
            {"v": -1.5},
            {"$natural": 1},
            {"$natural": 2},
            {"v": 1, "_id": -1},
        ]
    ]
    + [
        {"find": "c", "projection": v, **SORTED}
        for v in [
            1,
            "x",
            None,
            [],
            {"v": 1},
            {"v": 0},
            {"v": 1, "t": 0},
            {"_id": 0},
            {},
            {"v": "x"},
            {"v": 2},
            {"v": True},
        ]
    ],
)
each(
    "find: the collection argument",
    [{"find": v} for v in [1, "", None, {}, "nope", "a.b", "$bad", "system.views", "cé"]]
    + [{"find": "nope", "batchSize": 0}, {"find": "nope", "sort": {"a": 1}, "limit": 1}],
)
each(
    "find: hint",
    [
        find({"v": 5}, hint=h, **SORTED)
        for h in [
            "v_1",
            {"v": 1},
            "nope",
            {"nope": 1},
            {"$natural": 1},
            {"$natural": -1},
            "$natural",
            1,
            None,
            [],
            {},
            "_id_",
            {"_id": 1},
            {"v": -1},
            {"$natural": 2},
            {"$natural": 1, "v": 1},
        ]
    ],
)
each(
    "find: min and max",
    [
        find(min={"v": 3}, hint={"v": 1}),
        find(max={"v": 5}, hint={"v": 1}),
        find(min={"v": 3}, max={"v": 5}, hint={"v": 1}),
        find(min={"v": 5}, max={"v": 3}, hint={"v": 1}),
        find(min={"v": 3}, max={"v": 3}, hint={"v": 1}),
        find(min={"v": 3}),
        find(max={"v": 3}),
        find(min={"v": 3}, hint={"_id": 1}),
        find(min={"_id": 3}, hint={"_id": 1}),
        find(min={"_id": 3}, max={"_id": 6}, hint="_id_"),
        find(min={"w": 3}, hint={"v": 1}),
        find(min={}, hint={"v": 1}),
        find(min=3, hint={"v": 1}),
        find(min={"v": 3}, hint={"$natural": 1}),
        find(min={"v": 3}, hint="v_1", sort={"v": -1}),
        find({"_id": {"$gte": 2}}, min={"v": 3}, hint={"v": 1}),
    ],
)
each(
    "find: returnKey and showRecordId",
    [
        find({"v": 5}, returnKey=True, hint={"v": 1}),
        find({"v": 5}, returnKey=True),
        find({"_id": 1}, returnKey=True),
        find(returnKey=1),
        find(returnKey="x"),
        find({"_id": {"$lte": 2}}, showRecordId=True, **SORTED),
        find({"_id": 1}, showRecordId=True, projection={"v": 1}),
        find({"_id": 1}, showRecordId=True, returnKey=True),
        find(showRecordId="x"),
        find({"_id": 1}, projection={"r": {"$meta": "recordId"}}),
        find({"v": 5}, projection={"k": {"$meta": "indexKey"}}, hint={"v": 1}, **SORTED),
        find({"_id": 1}, projection={"k": {"$meta": "bogus"}}),
    ],
)
each(
    "find: the cursor flags",
    [
        find({"_id": 1}, **{flag: v})
        for flag in [
            "noCursorTimeout",
            "allowPartialResults",
            "allowDiskUse",
            "tailable",
            "awaitData",
            "readOnce",
        ]
        for v in [True, False, 1, "x", None]
    ],
)
each(
    "find: other arguments",
    [find({"_id": 1}, comment=v) for v in ["x", 1, {"a": 1}, None, [1]]]
    + [find({"_id": 1}, maxTimeMS=v) for v in [0, 1000, -1, "x", 1.5, None]]
    + [
        find({"$expr": {"$eq": ["$_id", "$$a"]}}, let=v)
        for v in [{"a": 1}, {}, 1, None, {"a": "$v"}, {"$a": 1}, {"A": 1}]
    ]
    + [
        find({"_id": 1}, **{k: 1})
        for k in [
            "bogus",
            "fields",
            "query",
            "orderby",
            "ntoreturn",
            "ntoskip",
            "max_time_ms",
            "term",
            "$hint",
            "explain",
            "snapshot",
            "maxScan",
            "partial",
        ]
    ]
    + [find({"_id": 1}, collation=v) for v in [{"locale": "en"}, {}, 1, None, {"locale": "zz"}]],
)
scenario(
    "find: a missing collection opens no cursor",
    {"find": "nope", "batchSize": 2},
    {"getMore": Int64(1), "collection": "nope"},
    {"count": "nope"},
    {"distinct": "nope", "key": "a"},
    {"aggregate": "nope", "pipeline": [], "cursor": {"batchSize": 0}},
    {"listCollections": 1, "nameOnly": True},
    seed=False,
)

# ---------------------------------------------------------------- getMore

scenario(
    "getMore: to the end, two at a time",
    find(batchSize=2, **SORTED) | {"save": "k"},
    more(batchSize=2),
    more(batchSize=2),
    more(batchSize=2),
    more(batchSize=2),
    more(batchSize=2),
)
scenario(
    "getMore: with no batchSize takes the rest",
    find(batchSize=2, **SORTED) | {"save": "k"},
    more(),
    more(),
)
scenario(
    "getMore: an exact fill leaves the cursor open",
    find(batchSize=2, **SORTED) | {"save": "k"},
    more(batchSize=6),
    more(batchSize=1),
    more(),
)
scenario(
    "getMore: after batchSize 0",
    find(batchSize=0, **SORTED) | {"save": "k"},
    more(batchSize=3),
    more(),
)
scenario(
    "getMore: under a limit",
    find(batchSize=2, limit=5, **SORTED) | {"save": "k"},
    more(batchSize=2),
    more(batchSize=2),
    more(),
)
scenario(
    "getMore: under a limit that a batch reaches",
    find(batchSize=2, limit=4, **SORTED) | {"save": "k"},
    more(batchSize=2),
    more(),
)
scenario(
    "getMore: under a skip", find(batchSize=2, skip=5, **SORTED) | {"save": "k"}, more(), more()
)
for value in BAD:
    scenario(
        f"getMore: batchSize {value!r}",
        find(batchSize=2, **SORTED) | {"save": "k"},
        more(batchSize=value),
        more(),
    )
for value in ["x", 2.5, None, True, {}, 5, Int64(5), 5.0, -1, Int64(0), Decimal128("5")]:
    scenario(
        f"getMore: cursor id {value!r}",
        find(batchSize=2, **SORTED) | {"save": "k"},
        {"getMore": value, "collection": "c"},
        more(),
    )
for value in ["nope", "", 1, None, {}, "c.x"]:
    scenario(
        f"getMore: collection {value!r}",
        find(batchSize=2, **SORTED) | {"save": "k"},
        {"getMore": Cur(), "collection": value},
        more(),
    )
scenario(
    "getMore: no collection",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"getMore": Cur()},
    more(),
)
scenario(
    "getMore: from another database",
    find(batchSize=2, **SORTED) | {"save": "k"},
    ("@other", more()),
    more(),
)
each(
    "getMore: other arguments",
    [find(batchSize=2, **SORTED) | {"save": "k"}]
    + [
        more(batchSize=1, **{k: v})
        for k, v in [
            ("maxTimeMS", 100),
            ("maxTimeMS", "x"),
            ("maxTimeMS", -1),
            ("comment", "c"),
            ("comment", {"a": 1}),
            ("bogus", 1),
            ("term", Int64(1)),
            ("filter", {}),
            ("limit", 1),
        ]
    ]
    + [more()],
)
scenario(
    "getMore: the writes it does and does not see",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"insert": "c", "documents": [{"_id": 9}]},
    {"delete": "c", "deletes": [{"q": {"_id": 5}, "limit": 1}]},
    {"update": "c", "updates": [{"q": {"_id": 6}, "u": {"$set": {"z": 1}}}]},
    more(),
)
scenario(
    "getMore: after the collection is dropped",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"drop": "c"},
    more(),
)
scenario(
    "getMore: after the collection is renamed",
    find(batchSize=2, **SORTED) | {"save": "k"},
    ("admin", {"renameCollection": "@db.c", "to": "@db.d"}),
    more(),
)
scenario(
    "getMore: after an index is dropped",
    find({"v": {"$gte": 0}}, batchSize=1, hint={"v": 1}) | {"save": "k"},
    {"dropIndexes": "c", "index": "v_1"},
    more(),
)
scenario(
    "getMore: after the database is dropped",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"dropDatabase": 1},
    more(),
)
scenario(
    "getMore: a sorted cursor",
    find(batchSize=2, sort={"v": -1, "_id": 1}) | {"save": "k"},
    more(batchSize=3),
    more(),
)
scenario(
    "getMore: a projected cursor",
    find(batchSize=2, projection={"v": 1, "_id": 0}, **SORTED) | {"save": "k"},
    more(),
)

# ------------------------------------------------------------ killCursors

scenario(
    "killCursors: then getMore",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"killCursors": "c", "cursors": [Cur()]},
    more(),
    {"killCursors": "c", "cursors": [Cur()]},
)
scenario(
    "killCursors: one live and one unknown",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"killCursors": "c", "cursors": [Int64(12345), Cur(), Int64(0), Int64(-5)]},
)
scenario(
    "killCursors: named for another collection",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"killCursors": "nope", "cursors": [Cur()]},
    more(),
)
scenario(
    "killCursors: from another database",
    find(batchSize=2, **SORTED) | {"save": "k"},
    ("@other", {"killCursors": "c", "cursors": [Cur()]}),
    more(),
)
scenario(
    "killCursors: the same cursor twice in one command",
    find(batchSize=2, **SORTED) | {"save": "k"},
    {"killCursors": "c", "cursors": [Cur(), Cur()]},
)
each(
    "killCursors: arguments",
    [
        {"killCursors": "c", "cursors": v}
        for v in [[], 1, None, "x", {}, [1], ["x"], [1.5], [None], [Int64(7), 7], [Decimal128("7")]]
    ]
    + [{"killCursors": v, "cursors": [Int64(7)]} for v in [1, "", None, "nope"]]
    + [
        {"killCursors": "c"},
        {"killCursors": "c", "cursors": [Int64(7)], "bogus": 1},
        {"killCursors": "c", "cursors": [Int64(7)], "comment": "x"},
    ],
)

# ------------------------------------------------ cursors of other commands

scenario(
    "aggregate: a cursor read on",
    agg({"$sort": {"_id": 1}}) | {"cursor": {"batchSize": 2}, "save": "k"},
    more(batchSize=3),
    more(),
)
scenario(
    "aggregate: batchSize 0",
    agg({"$sort": {"_id": 1}}) | {"cursor": {"batchSize": 0}, "save": "k"},
    more(),
)
scenario(
    "aggregate: batchSize 0 defers an error",
    agg({"$project": {"x": {"$divide": [1, "$bogus"]}}})
    | {"cursor": {"batchSize": 0}, "save": "k"},
    more(),
    agg({"$bogus": {}}) | {"cursor": {"batchSize": 0}},
)
scenario(
    "aggregate: $limit and batchSize",
    agg({"$sort": {"_id": 1}}, {"$limit": 2}) | {"cursor": {"batchSize": 2}, "save": "k"},
    more(),
)
each(
    "aggregate: the cursor argument",
    [
        {"aggregate": "c", "pipeline": [{"$sort": {"_id": 1}}], **extra}
        for extra in [
            {},
            {"cursor": 1},
            {"cursor": None},
            {"cursor": []},
            {"cursor": {"batchSize": "x"}},
            {"cursor": {"batchSize": -1}},
            {"cursor": {"batchSize": 2.5}},
            {"cursor": {"batchSize": 2.0}},
            {"cursor": {"batchSize": None}},
            {"cursor": {"bogus": 1}},
            {"cursor": {"batchSize": Int64(3)}},
            {"cursor": {}, "explain": False},
            {"cursor": {"batchSize": 1}, "singleBatch": True},
        ]
    ],
)
scenario(
    "listIndexes: a cursor read on",
    {
        "createIndexes": "c",
        "indexes": [{"key": {"a": 1}, "name": "a_1"}, {"key": {"b": 1}, "name": "b_1"}],
    },
    {"listIndexes": "c", "cursor": {"batchSize": 2}, "save": "k"},
    more(batchSize=1),
    more(),
)
each(
    "listIndexes: the cursor argument",
    [
        {"listIndexes": "c", **extra}
        for extra in [
            {"cursor": {"batchSize": 0}},
            {"cursor": {"batchSize": -1}},
            {"cursor": {"batchSize": "x"}},
            {"cursor": 1},
            {"cursor": {}},
            {"cursor": {"bogus": 1}},
            {"bogus": 1},
        ]
    ]
    + [{"listIndexes": v} for v in ["nope", 1, "", None]],
)
# mongod lists collections in no order a client can rely on, so the cursor is
# read over a listing of one.
scenario(
    "listCollections: a cursor read on",
    {
        "listCollections": 1,
        "nameOnly": True,
        "filter": {"name": "c"},
        "cursor": {"batchSize": 0},
        "save": "k",
    },
    {"getMore": Cur(), "collection": "$cmd.listCollections", "batchSize": 1},
    {"getMore": Cur(), "collection": "$cmd.listCollections"},
    {"getMore": Cur(), "collection": "c"},
)
each(
    "listCollections: the cursor argument",
    [
        {"listCollections": 1, "nameOnly": True, **extra}
        for extra in [
            {"cursor": {"batchSize": 0}},
            {"cursor": {"batchSize": -1}},
            {"cursor": {"batchSize": "x"}},
            {"cursor": 1},
            {"cursor": {"bogus": 1}},
            {"bogus": 1},
            {"filter": 1},
            {"filter": {"name": "c"}},
            {"nameOnly": "x"},
            {"authorizedCollections": True},
        ]
    ],
)

# ----------------------------------------------------------------- tailable

CAPPED = [
    {"create": "cap", "capped": True, "size": 4096},
    {"insert": "cap", "documents": [{"_id": 1}, {"_id": 2}, {"_id": 3}]},
]
TAIL = {"getMore": Cur(), "collection": "cap"}
scenario(
    "tailable: reads to the end and stays open",
    *CAPPED,
    {"find": "cap", "tailable": True, "batchSize": 2, "save": "k"},
    TAIL,
    TAIL,
    {"insert": "cap", "documents": [{"_id": 4}]},
    TAIL,
    TAIL,
    {"killCursors": "cap", "cursors": [Cur()]},
    TAIL,
    seed=False,
)
scenario(
    "tailable: awaitData waits for maxTimeMS",
    *CAPPED,
    {"find": "cap", "tailable": True, "awaitData": True, "save": "k"},
    TAIL | {"maxTimeMS": 200},
    {"insert": "cap", "documents": [{"_id": 4}]},
    TAIL | {"maxTimeMS": 200},
    seed=False,
)
scenario(
    "tailable: an empty capped collection",
    {"create": "cap", "capped": True, "size": 4096},
    {"find": "cap", "tailable": True, "save": "k"},
    TAIL,
    {"insert": "cap", "documents": [{"_id": 1}]},
    TAIL,
    seed=False,
)
scenario(
    "tailable: a filter",
    *CAPPED,
    {"find": "cap", "filter": {"_id": {"$gte": 3}}, "tailable": True, "save": "k"},
    {"insert": "cap", "documents": [{"_id": 4}, {"_id": 0}]},
    TAIL,
    seed=False,
)
scenario(
    "tailable: the collection is dropped",
    *CAPPED,
    {"find": "cap", "tailable": True, "save": "k"},
    {"drop": "cap"},
    TAIL,
    seed=False,
)
each(
    "tailable: what it refuses",
    [
        find(tailable=True),
        find(awaitData=True),
        {"find": "nope", "tailable": True},
        *CAPPED,
        {"find": "cap", "tailable": True, "sort": {"_id": 1}},
        {"find": "cap", "tailable": True, "sort": {"$natural": 1}},
        {"find": "cap", "tailable": True, "sort": {"$natural": -1}},
        {"find": "cap", "tailable": True, "limit": 1},
        {"find": "cap", "tailable": True, "singleBatch": True},
        {"find": "cap", "tailable": True, "skip": 1},
        {"find": "cap", "tailable": True, "batchSize": 0},
        {"find": "cap", "awaitData": True},
        {"find": "cap", "tailable": True, "awaitData": True, "maxTimeMS": 100},
        {"find": "cap", "tailable": True, "hint": {"_id": 1}},
        {"find": "cap", "tailable": True, "projection": {"_id": 0}},
        {"aggregate": "cap", "pipeline": [], "cursor": {}, "tailable": True},
    ],
)
scenario(
    "maxTimeMS on a getMore of an ordinary cursor",
    find(batchSize=2, **SORTED) | {"save": "k"},
    more(batchSize=1, maxTimeMS=100),
    find(batchSize=2, maxTimeMS=1000, **SORTED) | {"save": "k"},
    more(batchSize=1, maxTimeMS=100),
)

# -------------------------------------------------------------------- count

each(
    "count: what it counts",
    [
        {"count": "c"},
        {"count": "c", "query": {"v": 5}},
        {"count": "c", "query": {"v": {"$gte": 3}}},
        {"count": "c", "query": {}, "limit": 3},
        {"count": "c", "query": {}, "skip": 3},
        {"count": "c", "skip": 3, "limit": 2},
        {"count": "c", "skip": 100},
        {"count": "c", "limit": 0},
        {"count": "c", "limit": -3},
        {"count": "c", "query": {"v": 5}, "limit": -1},
        {"count": "c", "skip": -1},
        {"count": "c", "query": {"v": 5}, "hint": "v_1"},
        {"count": "c", "query": {"v": 5}, "hint": {"v": 1}},
        {"count": "c", "query": {"v": 5}, "hint": "nope"},
        {"count": "c", "hint": "nope"},
        {"count": "c", "query": {"v": 5}, "hint": {"$natural": 1}},
        {"count": "c", "query": {"$bogus": 1}},
        {"count": "c", "query": {"v": "X"}, "collation": {"locale": "en", "strength": 2}},
        {"count": "nope"},
        {"count": "nope", "query": {"a": 1}},
    ],
)
each(
    "count: arguments",
    [{"count": v} for v in [1, "", None, {}, "a.b"]]
    + [{"count": "c", "query": v} for v in [1, "x", None, []]]
    + [
        {"count": "c", "limit": v}
        for v in ["x", 2.5, None, True, {}, Int64(2), 2.0, Decimal128("2")]
    ]
    + [{"count": "c", "skip": v} for v in ["x", 2.5, None, True, {}, Int64(2), 2.0]]
    + [{"count": "c", "hint": v} for v in [1, None, [], {}]]
    + [
        {"count": "c", **{k: 1}}
        for k in ["bogus", "fields", "filter", "sort", "batchSize", "comment"]
    ]
    + [{"count": "c", "collation": v} for v in [1, {}, {"locale": "zz"}]]
    + [{"count": "c", "maxTimeMS": "x"}, {"count": "c", "readConcern": {"level": "majority"}}],
)

# ----------------------------------------------------------------- distinct


def distinct(key, **kw):
    return {"distinct": "c", "key": key, **kw}


each(
    "distinct: what it returns",
    [
        distinct("v"),
        distinct("_id"),
        distinct("t"),
        distinct("n.k"),
        distinct("n"),
        distinct("nope"),
        distinct("v", query={"_id": {"$lte": 3}}),
        distinct("v", query={"_id": 99}),
        distinct("v", query={"$bogus": 1}),
        distinct("t.0"),
        distinct("n.0.k"),
        distinct("v.x"),
        distinct("v", hint="v_1"),
        distinct("v", hint={"v": 1}),
        distinct("v", hint="nope"),
        distinct("v", hint={"$natural": 1}),
        distinct("v", collation={"locale": "en", "strength": 2}),
        {"distinct": "nope", "key": "v"},
    ],
)
scenario(
    "distinct: equal values of different types",
    {
        "insert": "m",
        "documents": [
            {"a": 1},
            {"a": 1.0},
            {"a": Int64(1)},
            {"a": Decimal128("1.0")},
            {"a": "a"},
            {"a": "A"},
            {"a": [1, 2]},
            {"a": [[1, 2]]},
            {"a": {"x": 1}},
            {"a": {"x": 1.0}},
            {"a": None},
            {},
            {"a": True},
            {"a": 0},
            {"a": -0.0},
            {"a": float("nan")},
            {"a": float("nan")},
            {"a": []},
        ],
    },
    {"distinct": "m", "key": "a"},
    {"distinct": "m", "key": "a", "collation": {"locale": "en", "strength": 2}},
    {"distinct": "m", "key": "a.x"},
    seed=False,
)
each(
    "distinct: arguments",
    [distinct(v) for v in [1, "", None, {}, [], "$v", "a..b", ".a", "a.", "a\x00b"]]
    + [{"distinct": "c"}]
    + [{"distinct": v, "key": "v"} for v in [1, "", None, {}]]
    + [distinct("v", query=v) for v in [1, "x", None, []]]
    + [
        distinct("v", **{k: 1})
        for k in ["bogus", "filter", "sort", "limit", "skip", "batchSize", "comment"]
    ]
    + [distinct("v", hint=v) for v in [1, None, [], {}]]
    + [distinct("v", collation=v) for v in [1, {}, {"locale": "zz"}]]
    + [distinct("v", maxTimeMS="x"), distinct("v", readConcern={"level": "majority"})],
)


# ---------------------------------------------------------------- running


def resolve(value, db, cursors):
    if isinstance(value, Cur):
        return Int64(cursors.get(value.label, 987654321))
    if isinstance(value, dict):
        return {k: resolve(v, db, cursors) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, db, cursors) for v in value]
    if isinstance(value, str) and "@db" in value:
        return value.replace("@db", db)
    return value


UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

#: The Rust server names the hint where mongod prints its plan; the wording is
#: a decision already made (`tests/test_rust_hint_error_message.py`), so an
#: unknown hint is compared by code.
UNKNOWN_HINT = "does not correspond to an existing index"


def clean(value, key=None):
    if isinstance(value, dict):
        if UNKNOWN_HINT in str(value.get("errmsg", "")):
            value = {**value, "errmsg": "<an unknown hint>"}
        out = {}
        for k, v in value.items():
            if k in NOISE:
                continue
            if k == "id" and key == "cursor":
                out[k] = "CURSOR" if v else 0
                continue
            if k in ("uuid", "$recordId") and key != "cursor":
                out[k] = "ID"
                continue
            out[k] = clean(v, k)
        return out
    if isinstance(value, list):
        if key in ("cursorsKilled", "cursorsNotFound", "cursorsAlive", "cursorsUnknown"):
            return ["CURSOR" if isinstance(v, int) and v > 10**6 else v for v in value]
        return [clean(v, key) for v in value]
    if isinstance(value, str):
        text = re.sub(r"cursor (id )?\d{6,}", r"cursor \1N", value)
        text = UUID_TEXT.sub("UUID", text)
        # mongod prints its parse of the query in front of a planner error.
        text = re.sub(r"Tree: .*?Proj: \{.*?\}\n", "", text, flags=re.DOTALL)
        # The order of the types a slot accepts varies from build to build.
        text = re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            text,
        )
        return re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(T)", text)
    if isinstance(value, float) and value != value:
        return "NaN"
    return value


def run_scenario(uri, index, steps):
    db = f"curp_{index}"
    wire = Wire(uri)
    cursors: dict[str, int] = {}
    replies = []
    for step in steps:
        where, command = step if isinstance(step, tuple) else (None, step)
        command = dict(command)
        save = command.pop("save", None)
        where = db if where is None else (db + "_o" if where == "@other" else where)
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
        replies.append(clean(reply))
    try:
        if cursors:
            wire.command(
                db, {"killCursors": "c", "cursors": [Int64(v) for v in cursors.values() if v]}
            )
        for name in (db, db + "_o"):
            wire.command(name, {"dropDatabase": 1})
    except (TimeoutError, ConnectionError, OSError):
        print(f"  NOTE: cleanup of {db} did not finish on {uri}", file=sys.stderr)
    finally:
        wire.close()
    return replies


def show(step):
    where, command = step if isinstance(step, tuple) else (None, step)
    body = {k: v for k, v in command.items() if k != "save"}
    return (("" if where is None else f"[{where}] ") + repr(body))[:200]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")

    # Self-check: the reference must page a cursor the way the probe assumes.
    check = run_scenario(
        mongod_uri, 0, [*SEED, find(batchSize=2, **SORTED) | {"save": "k"}, more()]
    )
    if not (check[2]["cursor"]["id"] == "CURSOR" and len(check[3]["cursor"]["nextBatch"]) == 6):
        print("self-check failed on mongod:", check, file=sys.stderr)
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
        print(f"\n--- {name}")
        for i in bad:
            print(f"  step {i}: {show(steps[i])}")
            print(f"    mongod: {str(want[i])[:700]}")
            print(f"    server: {str(got[i])[:700]}")
    print(
        f"\n=== cursor commands: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
