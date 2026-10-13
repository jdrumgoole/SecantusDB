"""Differential-probe the ORDER `find` returns documents in against a real mongod.

A query with no sort has no promised order, and a client still sees one: the
order of the index the server read, or of the collection. The same order
decides which document a `limit: 1` delete removes, which one an update
without `multi` changes, and which one `findAndModify` returns. So an order
that differs from mongod's is a different document written.

Each scenario seeds one collection (documents, then indexes, then any
`after` commands) and runs its steps over it:

- a READ step is compared by the `_id`s it returns, in order. A difference
  is tagged `order` when both servers returned the same documents and `set`
  when they did not.
- a WRITE step runs over a collection seeded afresh, and is compared by its
  reply and by what the collection holds afterwards.

Commands go over a raw connection.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/find_order.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import datetime
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Decimal128, Int64, MaxKey, MinKey, ObjectId, Timestamp  # noqa: E402
from bson.errors import InvalidBSON  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
RUN = f"{os.getpid() % 100000}"

DROP = {"$clusterTime", "operationTime", "opTime", "electionId"}
DAY = datetime.datetime(2020, 1, 2)

S: list[dict] = []


def scenario(name, docs, indexes, steps, after=()):
    S.append(
        {
            "name": name,
            "docs": docs,
            "indexes": list(indexes),
            "steps": list(steps),
            "after": list(after),
        }
    )


def index(key, **options):
    name = options.pop("name", "_".join(f"{k}_{v}" for k, v in key.items()))
    return {"key": key, "name": name, **options}


def find(filter_=None, **top):
    return ("r", {"find": "c", "filter": filter_ or {}, **top})


def agg(*pipeline, **top):
    return ("r", {"aggregate": "c", "pipeline": list(pipeline), "cursor": {}, **top})


def delete_one(filter_, **statement):
    return ("w", {"delete": "c", "deletes": [{"q": filter_, "limit": 1, **statement}]})


def update_one(filter_, **statement):
    return (
        "w",
        {"update": "c", "updates": [{"q": filter_, "u": {"$set": {"hit": 1}}, **statement}]},
    )


def modify(filter_, **top):
    return ("w", {"findAndModify": "c", "query": filter_, "update": {"$set": {"hit": 1}}, **top})


def remove(filter_, **top):
    return ("w", {"findAndModify": "c", "query": filter_, "remove": True, **top})


# ------------------------------------------------------------------ the data

# `a` repeats values, mixes types and holds arrays, and the documents are
# inserted in an order that is neither the order of `a` nor of `_id`.
A_VALUES = [
    (11, 5),
    (4, 3),
    (17, "b"),
    (2, 5),
    (9, None),
    (14, ...),
    (6, 3.0),
    (20, [1, 9]),
    (1, "a"),
    (13, 7),
    (8, True),
    (16, {"x": 1}),
    (3, 5),
    (19, Int64(3)),
    (7, [5, 3]),
    (12, 1),
    (5, DAY),
    (18, "B"),
    (10, []),
    (15, 9.5),
]


def mixed():
    docs = []
    for place, (id_, a) in enumerate(A_VALUES):
        doc = {"_id": id_}
        if a is not ...:
            doc["a"] = a
        doc["b"] = place % 4
        doc["c"] = (place * 7) % 5
        docs.append(doc)
    return docs


MIXED = mixed()

FILTERS = [
    {},
    {"a": 5},
    {"a": 3},
    {"a": {"$gt": 3}},
    {"a": {"$gte": 3}},
    {"a": {"$lt": 7}},
    {"a": {"$lte": 5}},
    {"a": {"$gt": 1, "$lt": 9}},
    {"a": {"$in": [5, 3]}},
    {"a": {"$in": [3, 5]}},
    {"a": {"$in": [7, "b", 5, None]}},
    {"a": {"$in": [9, 1]}},
    {"a": {"$ne": 5}},
    {"a": {"$nin": [5, 3]}},
    {"a": {"$exists": True}},
    {"a": {"$exists": False}},
    {"a": None},
    {"a": {"$type": "string"}},
    {"a": {"$type": "number"}},
    {"a": {"$regex": "^b"}},
    {"a": {"$regex": "b", "$options": "i"}},
    {"a": {"$not": {"$gt": 3}}},
    {"a": {"$mod": [2, 1]}},
    {"a": {"$gt": "a"}},
    {"a": {"$gte": None}},
    {"a": {"$size": 2}},
    {"a": {"$all": [5, 3]}},
    {"a": {"$elemMatch": {"$gt": 2}}},
    {"a": [1, 9]},
    {"a": True},
    {"a": {"x": 1}},
    {"a": {"$gt": {}}},
    {"a": {"$gte": MinKey()}},
    {"a": {"$lte": MaxKey()}},
    {"a": {"$gt": 0}, "b": 1},
    {"a": 5, "b": {"$gte": 1}},
    {"a": {"$in": [5, 3]}, "b": {"$lt": 3}},
    {"a": 5, "c": {"$gt": 1}},
    {"b": 2},
    {"b": {"$gte": 2}},
    {"b": {"$in": [3, 0]}},
    {"$or": [{"a": 5}, {"a": 3}]},
    {"$or": [{"a": 3}, {"a": 5}]},
    {"$or": [{"a": 7}, {"b": 1}]},
    {"$or": [{"b": 1}, {"a": 7}]},
    {"$or": [{"a": {"$gt": 6}}, {"a": {"$lt": 3}}]},
    {"$and": [{"a": {"$gt": 1}}, {"a": {"$lt": 6}}]},
    {"$nor": [{"a": 5}]},
    {"$expr": {"$eq": ["$a", 5]}},
    {"$expr": {"$gt": ["$a", 3]}},
]
SORTS = [
    {"a": 1},
    {"a": -1},
    {"b": 1},
    {"b": -1},
    {"c": 1},
    {"a": 1, "b": 1},
    {"a": 1, "b": -1},
    {"a": -1, "b": -1},
    {"b": 1, "a": 1},
    {"b": 1, "a": -1},
    {"_id": -1},
    {"$natural": -1},
]
SORTED = [
    {},
    {"a": 5},
    {"a": {"$gt": 1, "$lt": 9}},
    {"a": {"$in": [5, 3]}},
    {"a": {"$in": [7, "b", 5, None]}},
    {"a": {"$exists": True}},
    {"a": {"$gt": 0}, "b": 1},
    {"b": {"$gte": 2}},
    {"$or": [{"a": 7}, {"b": 1}]},
]
PICKS = [
    {},
    {"a": 5},
    {"a": {"$gt": 3}},
    {"a": {"$lt": 7}},
    {"a": {"$in": [5, 3]}},
    {"a": {"$in": [7, "b", 5, None]}},
    {"a": {"$exists": True}},
    {"a": {"$ne": 5}},
    {"a": {"$type": "string"}},
    {"a": {"$gt": 0}, "b": 1},
    {"b": {"$gte": 2}},
    {"$or": [{"a": 7}, {"b": 1}]},
    {"$or": [{"a": {"$gt": 6}}, {"a": {"$lt": 3}}]},
]


def reads():
    return [
        *[find(f) for f in FILTERS],
        *[find(f, sort=s) for f in SORTED for s in SORTS],
        *[find(f, limit=1) for f in PICKS],
        *[find(f, skip=2, limit=3) for f in PICKS],
        *[find(f, projection={"a": 1, "_id": 0}) for f in PICKS[:5]],
        *[find(f, sort={"a": 1}, limit=2) for f in PICKS[:7]],
        *[find(f, sort={"a": -1}, skip=1, limit=2) for f in PICKS[:7]],
    ]


def writes():
    return [
        *[delete_one(f) for f in PICKS],
        *[update_one(f) for f in PICKS],
        *[modify(f) for f in PICKS],
        *[remove(f) for f in PICKS],
    ]


CONFIGS = [
    ("no index", []),
    ("{a: 1}", [index({"a": 1})]),
    ("{a: -1}", [index({"a": -1})]),
    ("{a: 1, b: 1}", [index({"a": 1, "b": 1})]),
    ("{a: 1, b: -1}", [index({"a": 1, "b": -1})]),
    ("{a: -1, b: 1}", [index({"a": -1, "b": 1})]),
    ("{b: 1, a: 1}", [index({"b": 1, "a": 1})]),
    ("{b: 1}", [index({"b": 1})]),
    ("sparse {a: 1}", [index({"a": 1}, sparse=True)]),
    ("partial {a: 1}", [index({"a": 1}, partialFilterExpression={"b": {"$gte": 1}})]),
    ("{a: 1} and {b: 1}", [index({"a": 1}), index({"b": 1})]),
]
for label, indexes in CONFIGS:
    scenario(f"read, {label}", MIXED, indexes, reads())
for label, indexes in CONFIGS[:5] + CONFIGS[7:8] + CONFIGS[10:]:
    scenario(f"write, {label}", MIXED, indexes, writes())

# -------------------------------------------------------------------- hints

HINTS = ["a_1", "b_1", "a_1_b_1", {"a": 1}, {"$natural": 1}, {"$natural": -1}, "_id_", {"_id": 1}]
THREE = [index({"a": 1}), index({"b": 1}), index({"a": 1, "b": 1})]
scenario(
    "read, hinted",
    MIXED,
    THREE,
    [
        *[find(f, hint=h) for f in PICKS for h in HINTS],
        *[find(f, hint=h, sort={"b": 1}) for f in PICKS[:4] for h in HINTS],
        *[find(f, hint=h, sort={"a": -1}) for f in PICKS[:4] for h in HINTS],
        *[find(f) for f in FILTERS],
    ],
)
scenario(
    "write, hinted",
    MIXED,
    THREE,
    [
        *[delete_one(f, hint=h) for f in PICKS[:6] for h in HINTS],
        *[update_one(f, hint=h) for f in PICKS[:6] for h in HINTS],
        *[modify(f, hint=h) for f in PICKS[:6] for h in HINTS],
        *[remove(f, hint=h) for f in PICKS[:6] for h in HINTS],
    ],
)
scenario(
    "write, sorted findAndModify",
    MIXED,
    [index({"a": 1})],
    [
        *[modify(f, sort=s) for f in PICKS[:7] for s in SORTS[:11]],
        *[remove(f, sort=s) for f in PICKS[:7] for s in SORTS[:11]],
        *[modify(f, new=True, fields={"a": 1}) for f in PICKS[:7]],
        *[modify({"a": 99}, upsert=True), modify({"a": 99}, upsert=True, new=True)],
    ],
)

# ---------------------------------------------------------------------- _id

IDS = [
    7,
    "b",
    3,
    2.5,
    Int64(9),
    "a",
    {"k": 1},
    None,
    True,
    DAY,
    ObjectId("0123456789abcdef01234567"),
    1,
    Decimal128("4"),
    {"k": 0},
    "A",
    Timestamp(5, 1),
    False,
    -1,
]
ID_DOCS = [{"_id": v, "n": place} for place, v in enumerate(IDS)]
ID_FILTERS = [
    {},
    {"_id": {"$in": [3, 1, 7]}},
    {"_id": {"$in": [7, 1, 3]}},
    {"_id": {"$in": ["b", 3, "a", None]}},
    {"_id": {"$gt": 1}},
    {"_id": {"$gte": 2.5}},
    {"_id": {"$lt": 7}},
    {"_id": {"$gt": "A"}},
    {"_id": {"$ne": 3}},
    {"_id": {"$nin": [3, 7]}},
    {"_id": {"$exists": True}},
    {"_id": {"$type": "number"}},
    {"_id": {"$type": "string"}},
    {"_id": {"$gte": MinKey()}},
    {"_id": {"$gt": {}}},
    {"_id": {"$regex": "^[ab]"}},
    {"_id": {"$regex": "a", "$options": "i"}},
    {"$or": [{"_id": 7}, {"_id": 1}]},
    {"$or": [{"_id": {"$gt": 5}}, {"_id": {"$lt": 2}}]},
    {"_id": {"$gt": 0}, "n": {"$gt": 3}},
    {"n": {"$gt": 3}},
]
scenario(
    "read, _id",
    ID_DOCS,
    [],
    [
        *[find(f) for f in ID_FILTERS],
        *[find(f, sort={"_id": 1}) for f in ID_FILTERS],
        *[find(f, sort={"_id": -1}) for f in ID_FILTERS],
        *[find(f, hint={"_id": 1}) for f in ID_FILTERS],
        *[find(f, hint={"$natural": -1}) for f in ID_FILTERS],
        *[find(f, limit=1) for f in ID_FILTERS],
        *[find(f, sort={"n": -1}) for f in ID_FILTERS[:6]],
    ],
)
scenario(
    "write, _id",
    ID_DOCS,
    [],
    [
        *[delete_one(f) for f in ID_FILTERS],
        *[update_one(f) for f in ID_FILTERS],
        *[modify(f) for f in ID_FILTERS],
        *[remove(f) for f in ID_FILTERS],
    ],
)

# ------------------------------------------------------- the collection's order

MOVED = [
    {"update": "c", "updates": [{"q": {"_id": 2}, "u": {"$set": {"pad": "x" * 4000}}}]},
    {"delete": "c", "deletes": [{"q": {"_id": {"$in": [11, 13]}}, "limit": 0}]},
    {"insert": "c", "documents": [{"_id": 13, "a": 5, "b": 0, "c": 0}, {"_id": 0, "a": 5, "b": 1}]},
    {"update": "c", "updates": [{"q": {"_id": 4}, "u": {"_id": 4, "a": 5, "b": 9}}]},
    {"update": "c", "updates": [{"q": {"_id": 12}, "u": {"$set": {"a": 5}}}]},
    {"update": "c", "updates": [{"q": {"_id": 99}, "u": {"$set": {"a": 5}}, "upsert": True}]},
    {"update": "c", "updates": [{"q": {"b": 3}, "u": {"$inc": {"c": 1}}, "multi": True}]},
]
for label, indexes in CONFIGS[:4]:
    scenario(
        f"read, after writes, {label}",
        MIXED,
        indexes,
        [
            *[find(f, projection={"pad": 0}) for f in FILTERS],
            *[find(f, projection={"pad": 0}, sort=s) for f in SORTED[:4] for s in SORTS],
        ],
        after=MOVED,
    )

# ---------------------------------------------------------------- multikey

TAGS = [
    {"_id": 5, "t": ["x", "m"], "p": [{"q": 3}, {"q": 1}], "n": 1},
    {"_id": 2, "t": ["a"], "p": [{"q": 2}], "n": 2},
    {"_id": 8, "t": "m", "p": {"q": 3}, "n": 1},
    {"_id": 1, "t": ["m", "a", "z"], "p": [{"q": 1}, {"q": 5}, {"r": 1}], "n": 3},
    {"_id": 7, "t": [], "p": [], "n": 2},
    {"_id": 3, "t": ["z", "z"], "p": [{"q": 5}, {"q": 5}], "n": 1},
    {"_id": 6, "n": 3},
    {"_id": 4, "t": [["m"], "b"], "p": [{"q": [1, 3]}], "n": 2},
    {"_id": 9, "t": None, "p": None, "n": 1},
    {"_id": 10, "t": ["b", "a"], "p": [{"q": 2}, {"q": 3}], "n": 3},
]
TAG_FILTERS = [
    {},
    {"t": "m"},
    {"t": "a"},
    {"t": {"$in": ["z", "a"]}},
    {"t": {"$in": ["a", "z"]}},
    {"t": {"$gt": "a"}},
    {"t": {"$gte": "a", "$lte": "m"}},
    {"t": {"$lt": "n"}},
    {"t": {"$all": ["m", "a"]}},
    {"t": {"$all": ["a", "m"]}},
    {"t": {"$elemMatch": {"$gt": "a", "$lt": "z"}}},
    {"t": {"$size": 2}},
    {"t": ["a"]},
    {"t": []},
    {"t": None},
    {"t": {"$exists": True}},
    {"t": {"$ne": "m"}},
    {"t": {"$nin": ["m", "a"]}},
    {"t": {"$regex": "^[mz]"}},
    {"t": {"$type": "array"}},
    {"t": "m", "n": 1},
    {"t": {"$in": ["m", "z"]}, "n": {"$gte": 2}},
    {"$or": [{"t": "z"}, {"t": "a"}]},
    {"$or": [{"t": "a"}, {"t": "z"}]},
    {"p.q": 3},
    {"p.q": {"$in": [5, 2]}},
    {"p.q": {"$gt": 1}},
    {"p.q": {"$gte": 1, "$lt": 5}},
    {"p": {"$elemMatch": {"q": {"$gt": 2}}}},
    {"p.q": None},
    {"p.q": {"$exists": True}},
    {"p.r": 1},
    {"n": {"$gte": 2}},
]
TAG_SORTS = [
    {"t": 1},
    {"t": -1},
    {"p.q": 1},
    {"p.q": -1},
    {"n": 1},
    {"n": 1, "t": 1},
    {"n": -1, "t": -1},
]
TAG_CONFIGS = [
    ("no index", []),
    ("{t: 1}", [index({"t": 1})]),
    ("{t: -1}", [index({"t": -1})]),
    ("{p.q: 1}", [index({"p.q": 1})]),
    ("{n: 1, t: 1}", [index({"n": 1, "t": 1})]),
    ("{t: 1, n: 1}", [index({"t": 1, "n": 1})]),
    ("{t: 1} and {p.q: 1}", [index({"t": 1}), index({"p.q": 1})]),
]
for label, indexes in TAG_CONFIGS:
    scenario(
        f"read, arrays, {label}",
        TAGS,
        indexes,
        [
            *[find(f) for f in TAG_FILTERS],
            *[find(f, sort=s) for f in TAG_FILTERS[:8] + TAG_FILTERS[24:28] for s in TAG_SORTS],
            *[find(f, limit=1) for f in TAG_FILTERS],
        ],
    )
for label, indexes in TAG_CONFIGS[:4]:
    scenario(
        f"write, arrays, {label}",
        TAGS,
        indexes,
        [
            *[delete_one(f) for f in TAG_FILTERS],
            *[update_one(f) for f in TAG_FILTERS],
            *[remove(f) for f in TAG_FILTERS],
        ],
    )

# --------------------------------------------------------------- aggregate

GROUPED = {"$group": {"_id": None, "first": {"$first": "$_id"}, "last": {"$last": "$_id"}}}
PUSHED = {"$group": {"_id": "$b", "ids": {"$push": "$_id"}}}
for label, indexes in CONFIGS[:4] + CONFIGS[10:]:
    scenario(
        f"aggregate, {label}",
        MIXED,
        indexes,
        [
            *[agg({"$match": f}) for f in PICKS],
            *[agg({"$match": f}, {"$limit": 2}) for f in PICKS],
            *[agg({"$match": f}, {"$skip": 1}, {"$limit": 2}) for f in PICKS],
            *[agg({"$match": f}, GROUPED) for f in PICKS],
            *[agg({"$match": f}, PUSHED, {"$sort": {"_id": 1}}) for f in PICKS],
            *[agg({"$match": f}, {"$sort": s}) for f in PICKS[:5] for s in SORTS[:11]],
            *[agg({"$match": f}, {"$sort": s}, GROUPED) for f in PICKS[:5] for s in SORTS[:6]],
            *[agg({"$sort": s}, {"$match": f}) for f in PICKS[:5] for s in SORTS[:6]],
            *[agg({"$sort": s}, {"$limit": 3}) for s in SORTS[:11]],
            *[agg({"$match": f}, {"$project": {"a": 1}}) for f in PICKS[:5]],
            *[
                agg({"$match": f}, {"$addFields": {"z": 1}}, {"$match": {"b": {"$gte": 1}}})
                for f in PICKS[:5]
            ],
            *[agg({"$match": f}, hint="a_1") for f in PICKS[:5]],
            *[agg({"$match": f}, hint={"$natural": -1}) for f in PICKS[:5]],
        ],
    )

# --------------------------------------------------------------- collation

WORDS = [
    {"_id": place, "w": w, "g": place % 2}
    for place, w in enumerate(["b", "A", "a", "B", "c", "á", "C", "ab", "Ab", "aB", "", None])
]
CI = {"locale": "en", "strength": 2}
WORD_FILTERS = [
    {},
    {"w": "a"},
    {"w": {"$in": ["b", "A"]}},
    {"w": {"$gt": "a"}},
    {"w": {"$gte": "A", "$lt": "c"}},
    {"w": {"$ne": "a"}},
    {"w": {"$exists": True}},
    {"w": {"$regex": "^a"}},
    {"w": "a", "g": 0},
]
for label, indexes in [
    ("no index", []),
    ("{w: 1}", [index({"w": 1})]),
    ("collated {w: 1}", [index({"w": 1}, collation=CI)]),
    ("{w: 1} both ways", [index({"w": 1}), index({"w": 1}, collation=CI, name="w_ci")]),
]:
    scenario(
        f"read, strings, {label}",
        WORDS,
        indexes,
        [
            *[find(f) for f in WORD_FILTERS],
            *[find(f, collation=CI) for f in WORD_FILTERS],
            *[find(f, sort={"w": 1}) for f in WORD_FILTERS],
            *[find(f, sort={"w": -1}) for f in WORD_FILTERS],
            *[find(f, sort={"w": 1}, collation=CI) for f in WORD_FILTERS],
            *[find(f, sort={"w": -1}, collation=CI) for f in WORD_FILTERS],
            *[find(f, sort={"g": 1}, collation=CI) for f in WORD_FILTERS],
        ],
    )

# ------------------------------------------------------------------ running

UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
OID = ObjectId("0123456789abcdef01234567")


def clean(value, db, key=None):
    if isinstance(value, dict):
        return {k: clean(v, db, k) for k, v in value.items() if k not in DROP}
    if isinstance(value, list):
        return [clean(v, db, key) for v in value]
    if isinstance(value, ObjectId):
        return value if value == OID else "OID"
    if isinstance(value, Int64):
        # An open cursor's id is the server's own choice.
        return "CURSOR" if key == "id" and int(value) != 0 else f"Int64({int(value)})"
    if isinstance(value, float):
        return "NaN" if value != value else f"double({value!r})"
    if isinstance(value, str):
        return UUID_TEXT.sub("UUID", value.replace(db, "@db"))
    return value


def send(wire, uri, db, command):
    """One command; a connection that fails is replaced and reported."""
    try:
        return wire, wire.command(db, command)
    except TimeoutError:
        wire.close()
        return Wire(uri), {"probe": "TIMEOUT"}
    except InvalidBSON as exc:
        return wire, {"probe": f"REPLY NOT DECODED {exc}"}
    except (ConnectionError, OSError) as exc:
        wire.close()
        return Wire(uri), {"probe": f"CONNECTION {type(exc).__name__}"}


def seed(wire, uri, db, spec):
    """Drop `c` and build it again; the reply that went wrong, if one did."""
    wire, _ = send(wire, uri, db, {"drop": "c"})
    wire, made = send(wire, uri, db, {"insert": "c", "documents": spec["docs"]})
    if made.get("n") != len(spec["docs"]) or made.get("writeErrors"):
        return wire, made
    if spec["indexes"]:
        wire, made = send(wire, uri, db, {"createIndexes": "c", "indexes": spec["indexes"]})
        if made.get("ok") != 1.0:
            return wire, made
    for command in spec["after"]:
        wire, made = send(wire, uri, db, command)
        if made.get("ok") != 1.0 or made.get("writeErrors"):
            return wire, made
    return wire, None


def ids(reply):
    """A cursor reply as the `_id`s it returned; any other reply whole."""
    batch = reply.get("cursor", {}).get("firstBatch")
    if batch is None:
        return {"reply": reply}
    return {"ids": [doc.get("_id", doc) if isinstance(doc, dict) else doc for doc in batch]}


def run_scenario(uri, number, spec):
    db = f"ford{RUN}_{number}"
    wire = Wire(uri)
    out = []
    wire, failed = seed(wire, uri, db, spec)
    dirty = False
    for kind, command in spec["steps"]:
        if failed is None and (dirty or kind == "w"):
            wire, failed = seed(wire, uri, db, spec)
            dirty = False
        if failed is not None:
            out.append({"probe": "SETUP", "reply": clean(failed, db)})
            failed, dirty = None, True
            continue
        wire, reply = send(wire, uri, db, command)
        if kind == "r":
            if "batchSize" not in command and "cursor" in reply:
                assert reply["cursor"]["id"] == 0, "a read left a cursor open"
            out.append(clean(ids(reply), db))
            continue
        dirty = True
        wire, after = send(wire, uri, db, {"find": "c", "batchSize": 500})
        out.append({"reply": clean(reply, db), "after": clean(after["cursor"]["firstBatch"], db)})
    wire, _ = send(wire, uri, db, {"dropDatabase": 1})
    wire.close()
    return out


def tag(want, got, command):
    """What kind of difference a read shows."""
    if "ids" not in want or "ids" not in got:
        return "reply"
    # mongod keeps the first `limit` documents of a sort in a heap, and the
    # order the heap leaves equal documents in is its own.
    stages = [next(iter(stage)) for stage in command.get("pipeline", [])]
    if ("sort" in command and "limit" in command) or (
        "$sort" in stages and "$limit" in stages[stages.index("$sort") :]
    ):
        return "sort with limit"
    same = sorted(map(repr, want["ids"])) == sorted(map(repr, got["ids"]))
    return "order" if same else "set"


def show(step):
    _, command = step
    name = next(iter(command))
    rest = {k: v for k, v in command.items() if k not in (name, "cursor")}
    for only in ("deletes", "updates"):
        if only in rest:
            rest = {k: v for k, v in rest[only][0].items() if k != "u"}
    return f"{name} {rest!r}"


def self_check(uri):
    """The reference must show an index's order where the probe expects it."""
    spec = {
        "docs": [{"_id": 1, "a": 2}, {"_id": 2, "a": 1}],
        "indexes": [index({"a": 1})],
        "after": [],
        "steps": [find(), find({"a": {"$gt": 0}}), delete_one({"a": {"$gt": 0}})],
    }
    got = run_scenario(uri, 0, spec)
    want = [{"ids": [1, 2]}, {"ids": [2, 1]}]
    if got[:2] != want or [d["_id"] for d in got[2].get("after", [])] != [1]:
        print("self-check failed on mongod:", got, file=sys.stderr)
        return False
    return True


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "900"))
    if not self_check(mongod_uri):
        return 2

    divergent = replies = replies_bad = total = 0
    kinds: dict[str, int] = {}
    for number, spec in enumerate(S, start=1):
        name, steps = spec["name"], spec["steps"]
        if only and only not in name:
            continue
        total += 1
        want = run_scenario(mongod_uri, number, spec)
        got = run_scenario(server_uri, number, spec)
        replies += len(steps)
        setups = [w for w in want if w.get("probe") == "SETUP"]
        if setups:
            print(f"\n--- {name}: {len(setups)} steps could not be set up on mongod")
            print(f"    {str(setups[0])[:width]}")
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
        if not bad:
            continue
        divergent += 1
        replies_bad += len(bad)
        print(f"\n--- {name} ({len(bad)} of {len(steps)})")
        for i in bad:
            w, g = want[i], got[i]
            if steps[i][0] == "r":
                kind = tag(w, g, steps[i][1])
                kinds[kind] = kinds.get(kind, 0) + 1
                print(f"  step {i} [{kind}]: {show(steps[i])}")
                print(f"    mongod: {str(w.get('ids', w))[:width]}")
                print(f"    server: {str(g.get('ids', g))[:width]}")
                continue
            kinds["write"] = kinds.get("write", 0) + 1
            print(f"  step {i} [write]: {show(steps[i])}")
            for part in ("reply", "after"):
                if repr(w.get(part)) != repr(g.get(part)):
                    for side, v in (("mongod", w.get(part)), ("server", g.get(part))):
                        shown = v
                        if part == "after" and isinstance(v, list):
                            shown = [(d["_id"], "hit") if "hit" in d else d["_id"] for d in v]
                        print(f"    {side} {part}: {str(shown)[:width]}")
    counted = ", ".join(f"{n} {k}" for k, n in sorted(kinds.items()))
    print(
        f"\n=== find order: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ({counted}) ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
