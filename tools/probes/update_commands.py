"""Differential-probe `update` and `findAndModify` against a real mongod.

The update operators, the positional operators and `arrayFilters`, pipeline
updates, replacements, upserts, and what each command accepts as an argument:
for every case the whole reply is compared AND the documents the collection
holds afterwards. A write that succeeds and stores a different document is
the failure this is here to find, and the reply alone does not show it.

A `case` runs against a collection of its own: the collection is dropped,
the seed documents are inserted, the command runs, and every document is read
back. Commands go over a raw connection, so an argument a driver would refuse
to send still reaches the server.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/update_commands.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import datetime
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Decimal128, Int64, MaxKey, MinKey, ObjectId, Timestamp  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
RUN = f"{os.getpid() % 100000}"

DROP = {"$clusterTime", "operationTime", "opTime", "electionId"}

BAD = ["x", 2.5, -1, None, True, {}, [1], Int64(2), 0, Decimal128("2"), 1]
BOOLS = [True, False, 1, 0, "x", None, {}, 2.5]
LONG_MAX = Int64(2**63 - 1)
INT_MAX = 2**31 - 1
DATE = datetime.datetime(2020, 1, 2, 3, 4, 5)

S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


def upd(q, u, **more):
    """One update statement; `top` holds fields for the command itself."""
    top = more.pop("top", {})
    return {"update": "c", "updates": [{"q": q, "u": u, **more}], **top}


def fam(**fields):
    return {"findAndModify": "c", **fields}


def case(seed, command):
    """Run `command` against a fresh collection holding `seed`."""
    return ("case", seed if isinstance(seed, list) else [seed], command)


def each(seed, *updates, q=None, **more):
    """One case per update document, each against the same seed."""
    return [case(seed, upd({} if q is None else q, u, **more)) for u in updates]


FIND = {"find": "c", "sort": {"_id": 1}}

# ------------------------------------------------------- the field operators

NUMS = {
    "_id": 1,
    "i": 5,
    "l": Int64(5),
    "d": 5.5,
    "m": Decimal128("5.5"),
    "s": "x",
    "n": None,
    "b": True,
    "a": [1, 2],
    "o": {"x": 1},
    "big": INT_MAX,
    "huge": LONG_MAX,
}

scenario(
    "$inc: what it adds to",
    *each(
        NUMS,
        *[{"$inc": {f: 1}} for f in ["i", "l", "d", "m", "s", "n", "b", "a", "o", "nope", "o.x"]],
        {"$inc": {"big": 1}},
        {"$inc": {"huge": 1}},
        {"$inc": {"huge": -1}},
        {"$inc": {"o.y.z": 2}},
        {"$inc": {"a.0": 10}},
        {"$inc": {"a.5": 10}},
        {"$inc": {"s.x": 1}},
        {"$inc": {"i": 0}},
        {"$inc": {"d": -0.0}},
    ),
)
scenario(
    "$inc: what it adds",
    *each(
        NUMS,
        *[
            {"$inc": {"i": v}}
            for v in [
                Int64(1),
                1.5,
                Decimal128("1.5"),
                "1",
                None,
                True,
                [1],
                {},
                float("nan"),
                float("inf"),
                INT_MAX,
                LONG_MAX,
                -6,
            ]
        ],
        {"$inc": {"l": 1.5}},
        {"$inc": {"l": Decimal128("1")}},
        {"$inc": {"d": Int64(1)}},
        {"$inc": {"m": 1}},
        {"$inc": {"m": 1.25}},
        {"$inc": {"nope": Int64(3)}},
        {"$inc": {"nope": 1.5}},
        {"$inc": {}},
        {"$inc": 1},
        {"$inc": {"i": 1, "l": 1, "d": 1}},
    ),
)
scenario(
    "$mul",
    *each(
        NUMS,
        *[{"$mul": {f: 2}} for f in ["i", "l", "d", "m", "s", "n", "b", "a", "nope", "big", "huge"]],
        *[
            {"$mul": {"i": v}}
            for v in [Int64(2), 1.5, Decimal128("1.5"), "2", None, 0, float("nan"), -1]
        ],
        {"$mul": {"nope": 1.5}},
        {"$mul": {"nope": Int64(2)}},
        {"$mul": {"nope": Decimal128("2")}},
        {"$mul": {"o.q": 3}},
        {"$mul": {"d": 0}},
        {"$mul": {}},
    ),
)
scenario(
    "$min and $max",
    *each(
        NUMS,
        *[
            {op: {f: v}}
            for op in ("$min", "$max")
            for f, v in [
                ("i", 3),
                ("i", 7),
                ("i", 5.0),
                ("i", Int64(5)),
                ("i", "a"),
                ("i", None),
                ("s", 1),
                ("s", "y"),
                ("n", 0),
                ("nope", 4),
                ("a", [1, 3]),
                ("a", 9),
                ("o", {"x": 0}),
                ("d", float("nan")),
                ("b", False),
                ("i", MinKey()),
                ("i", MaxKey()),
                ("o.x", 0),
                ("o.z", 0),
            ]
        ],
    ),
)
scenario(
    "$bit",
    *each(
        NUMS,
        *[
            {"$bit": {f: {op: v}}}
            for f in ("i", "l")
            for op in ("and", "or", "xor")
            for v in (3, Int64(3))
        ],
        {"$bit": {"d": {"and": 1}}},
        {"$bit": {"s": {"and": 1}}},
        {"$bit": {"nope": {"or": 6}}},
        {"$bit": {"nope": {"and": 6}}},
        {"$bit": {"i": {"and": 1.5}}},
        {"$bit": {"i": {"nand": 1}}},
        {"$bit": {"i": {}}},
        {"$bit": {"i": 1}},
        {"$bit": {"i": {"and": 1, "or": 8}}},
        {"$bit": {"i": {"and": "1"}}},
        {"$bit": {"m": {"and": 1}}},
    ),
)
scenario(
    "$set and $unset: the path",
    *each(
        NUMS,
        *[
            {"$set": {p: 9}}
            for p in [
                "o.x",
                "o.y",
                "o.y.z",
                "a.0",
                "a.3",
                "a.x",
                "a.0.x",
                "s.x",
                "n.x",
                "i.x",
                "",
                "a..b",
                ".a",
                "a.",
                "$x",
                "o.$x",
                "x.$y",
                "a.-1",
                "a.01",
                "a.1.5",
                "new.a.b.c",
                "_id",
            ]
        ],
        *[
            {"$unset": {p: ""}}
            for p in ["i", "o.x", "o.nope", "a.0", "a.9", "nope", "s.x", "_id", "", "a.x", "o"]
        ],
        {"$unset": {"i": 1, "l": True, "d": None}},
        {"$unset": {}},
        {"$unset": "i"},
        {"$unset": ["i"]},
        {"$set": {}},
        {"$set": "i"},
        {"$set": None},
        {"$set": {"i": 5}},
        {"$set": {"i": Int64(5)}},
        {"$set": {"i": 5.0}},
        {"$set": {"d": 5.5}},
        {"$set": {"o": {"x": 1}}},
        {"$set": {"o": {"x": Int64(1)}}},
        {"$set": {"a": [1, 2]}},
        {"$set": {"z": {"$a": 1}}},
        {"$set": {"z": {"a.b": 1}}},
        {"$set": {"z": {"": 1}}},
        {"$set": {"z": -0.0}},
    ),
)
scenario(
    "$rename",
    *each(
        NUMS,
        *[
            {"$rename": {a: b}}
            for a, b in [
                ("i", "j"),
                ("i", "l"),
                ("i", "i"),
                ("nope", "j"),
                ("nope", "l"),
                ("i", "o.i"),
                ("o.x", "x"),
                ("o.x", "o.y"),
                ("o", "o.o"),
                ("o.x", "o"),
                ("a.0", "z"),
                ("i", "a.0"),
                ("i", "a.x"),
                ("i", "s.x"),
                ("s.x", "z"),
                ("_id", "z"),
                ("i", "_id"),
                ("i", ""),
                ("", "z"),
                ("i", "$z"),
                ("i", "z.$"),
                ("i", "q.r.s"),
                ("n", "z"),
            ]
        ],
        {"$rename": {"i": 1}},
        {"$rename": {"i": None}},
        {"$rename": {"i": "j", "l": "k"}},
        {"$rename": {"i": "j", "j": "k"}},
        {"$rename": {"i": "j", "l": "j"}},
        {"$rename": {}},
        {"$rename": "i"},
        {"$rename": {"i": "j"}, "$set": {"j": 1}},
        {"$rename": {"i": "j"}, "$set": {"i": 1}},
    ),
)
scenario(
    "$currentDate",
    *each(
        NUMS,
        *[
            {"$currentDate": {"t": v}}
            for v in [
                True,
                False,
                1,
                "date",
                None,
                {},
                {"$type": "date"},
                {"$type": "timestamp"},
                {"$type": "Date"},
                {"$type": 9},
                {"$type": "date", "x": 1},
                {"type": "date"},
                [],
            ]
        ],
        {"$currentDate": {"o.t": True, "i": {"$type": "timestamp"}}},
        {"$currentDate": {"a.0": True}},
        {"$currentDate": {"s.x": True}},
        {"$currentDate": {}},
        {"$currentDate": 1},
    ),
)
scenario(
    "operators together",
    *each(
        NUMS,
        {"$set": {"i": 1}, "$inc": {"i": 1}},
        {"$set": {"o": 1}, "$inc": {"o.x": 1}},
        {"$set": {"o.x": 1}, "$unset": {"o": ""}},
        {"$set": {"o.x": 1, "o.y": 2}, "$inc": {"o.z": 1}},
        {"$set": {"a.0": 1}, "$push": {"a": 3}},
        {"$inc": {"i": 1}, "$mul": {"l": 2}, "$min": {"d": 1}, "$max": {"m": 99}},
        {"$set": {"z": 1}, "y": 2},
        {"y": 2, "$set": {"z": 1}},
        {"$nope": {"z": 1}},
        {"$set": {"z": 1}, "$nope": {"y": 1}},
        {"$Set": {"z": 1}},
        {"$setOnInsert": {"z": 1}},
        {"$setOnInsert": {"z": 1}, "$set": {"y": 1}},
        {"$setOnInsert": {}},
        {"$setOnInsert": 1},
        {"$set": {"x.y": 1, "x": {"z": 2}}},
        {"$set": {"z": 1}, "$unset": {"z": ""}},
        {"$pushAll": {"a": [1]}},
        {"$isolated": 1},
    ),
)

# -------------------------------------------------------- the array operators

ARR = {
    "_id": 1,
    "a": [3, 1, 2],
    "s": ["b", "a"],
    "o": [{"k": 2, "v": "x"}, {"k": 1, "v": "y"}],
    "e": [],
    "n": 5,
    "nn": [[1, 2], [3]],
    "mix": [1, "a", None, 2.5, {"x": 1}, [1]],
}

scenario(
    "$push",
    *each(
        ARR,
        {"$push": {"a": 9}},
        {"$push": {"a": [9]}},
        {"$push": {"a": None}},
        {"$push": {"n": 1}},
        {"$push": {"nope": 1}},
        {"$push": {"x.y": 1}},
        {"$push": {"o.0.k": 1}},
        {"$push": {"nn.0": 9}},
        {"$push": {"a": {"$each": [7, 8]}}},
        {"$push": {"a": {"$each": []}}},
        {"$push": {"a": {"$each": 7}}},
        {"$push": {"a": {"$each": [7], "$nope": 1}}},
        {"$push": {"a": {"$each": [7], "x": 1}}},
        {"$push": {"a": {"x": 1, "$each": [7]}}},
        {"$push": {"a": {"$position": 0}}},
        {"$push": {"a": {"$slice": 1}}},
        {"$push": {"a": {"$sort": 1}}},
        *[
            {"$push": {"a": {"$each": [7, 8], "$position": p}}}
            for p in [0, 1, 99, -1, -99, 1.0, 1.5, "0", None, Int64(1)]
        ],
        *[
            {"$push": {"a": {"$each": [7, 8], "$slice": p}}}
            for p in [0, 2, -2, 99, -99, 2.0, 2.5, "2", None, Int64(2)]
        ],
        *[
            {"$push": {"a": {"$each": [0], "$sort": p}}}
            for p in [1, -1, 0, 2, 1.0, "1", None, {}, {"x": 1}, True]
        ],
        {"$push": {"o": {"$each": [{"k": 0, "v": "z"}], "$sort": {"k": 1}}}},
        {"$push": {"o": {"$each": [{"k": 0, "v": "z"}], "$sort": {"k": -1}}}},
        {"$push": {"o": {"$each": [], "$sort": {"v": 1, "k": -1}}}},
        {"$push": {"o": {"$each": [], "$sort": {"k": 2}}}},
        {"$push": {"o": {"$each": [], "$sort": {"nope": 1}}}},
        {"$push": {"o": {"$each": [], "$sort": {"": 1}}}},
        {"$push": {"o": {"$each": [], "$sort": 1}}},
        {"$push": {"mix": {"$each": [], "$sort": 1}}},
        {"$push": {"mix": {"$each": [], "$sort": -1}}},
        {"$push": {"mix": {"$each": [], "$sort": {"x": 1}}}},
        {"$push": {"a": {"$each": [0], "$sort": 1, "$slice": 2, "$position": 0}}},
        {"$push": {"a": {"$each": [0], "$sort": -1, "$slice": -2}}},
        {"$push": {"a": 1, "s": "c", "e": {"$each": [1, 2]}}},
        {"$push": {}},
        {"$push": 1},
    ),
)
scenario(
    "$addToSet",
    *each(
        ARR,
        {"$addToSet": {"a": 1}},
        {"$addToSet": {"a": 1.0}},
        {"$addToSet": {"a": Int64(1)}},
        {"$addToSet": {"a": 9}},
        {"$addToSet": {"a": [1]}},
        {"$addToSet": {"n": 1}},
        {"$addToSet": {"nope": 1}},
        {"$addToSet": {"a": {"$each": [1, 9, 9, 8]}}},
        {"$addToSet": {"a": {"$each": []}}},
        {"$addToSet": {"a": {"$each": 1}}},
        {"$addToSet": {"a": {"$each": [9], "$position": 0}}},
        {"$addToSet": {"a": {"$each": [9], "$slice": 1}}},
        {"$addToSet": {"a": {"x": 1, "$each": [9]}}},
        {"$addToSet": {"o": {"k": 2, "v": "x"}}},
        {"$addToSet": {"o": {"v": "x", "k": 2}}},
        {"$addToSet": {"o": {"k": 2}}},
        {"$addToSet": {"mix": None}},
        {"$addToSet": {"mix": {"x": 1.0}}},
        {"$addToSet": {"e": {"$each": [1, 1.0, Int64(1), "1"]}}},
        {"$addToSet": {"e": {"$each": [0.0, -0.0, float("nan"), float("nan")]}}},
        {"$addToSet": {"x.y": 1}},
        {"$addToSet": {}},
    ),
)
scenario(
    "$pop, $pull and $pullAll",
    *each(
        ARR,
        *[{"$pop": {"a": v}} for v in [1, -1, 0, 2, 1.0, 1.5, "1", None, True, Int64(-1)]],
        {"$pop": {"e": 1}},
        {"$pop": {"n": 1}},
        {"$pop": {"nope": 1}},
        {"$pop": {"nn.0": -1}},
        {"$pop": {}},
        {"$pull": {"a": 1}},
        {"$pull": {"a": 1.0}},
        {"$pull": {"a": 9}},
        {"$pull": {"a": {"$gt": 1}}},
        {"$pull": {"a": {"$in": [1, 2]}}},
        {"$pull": {"a": {"$nope": 1}}},
        {"$pull": {"a": [3, 1, 2]}},
        {"$pull": {"nn": [3]}},
        {"$pull": {"nn": {"$in": [[3]]}}},
        {"$pull": {"o": {"k": 2}}},
        {"$pull": {"o": {"k": {"$gte": 1}}}},
        {"$pull": {"o": {"v": "y", "k": 1}}},
        {"$pull": {"o": {"$or": [{"k": 1}, {"v": "x"}]}}},
        {"$pull": {"o": {"nope": None}}},
        {"$pull": {"o": {}}},
        {"$pull": {"mix": None}},
        {"$pull": {"mix": {"$type": "string"}}},
        {"$pull": {"mix": {"x": 1}}},
        {"$pull": {"mix": {"$elemMatch": {"$eq": 1}}}},
        {"$pull": {"s": {"$regex": "^a"}}},
        {"$pull": {"n": 1}},
        {"$pull": {"nope": 1}},
        {"$pull": {"e": 1}},
        {"$pull": {}},
        {"$pull": {"a": {"$gt": 1}, "s": "a"}},
        {"$pullAll": {"a": [1, 2]}},
        {"$pullAll": {"a": [1.0, 9]}},
        {"$pullAll": {"a": []}},
        {"$pullAll": {"a": 1}},
        {"$pullAll": {"n": [1]}},
        {"$pullAll": {"nope": [1]}},
        {"$pullAll": {"o": [{"k": 2, "v": "x"}]}},
        {"$pullAll": {"o": [{"v": "x", "k": 2}]}},
        {"$pullAll": {"mix": [None, [1]]}},
    ),
)

# ------------------------------------------------------------ the positionals

POS = {
    "_id": 1,
    "a": [1, 2, 3, 2],
    "o": [{"k": 1, "t": [1, 2]}, {"k": 2, "t": [3, 4]}, {"k": 2, "t": []}],
    "b": [5, 6],
    "n": 7,
    "nn": [[1, 2], [3, 4]],
}

scenario(
    "the positional operator",
    *[
        case(POS, upd(q, u))
        for q, u in [
            ({"a": 2}, {"$set": {"a.$": 9}}),
            ({"a": {"$gt": 1}}, {"$inc": {"a.$": 10}}),
            ({"a": {"$in": [3, 2]}}, {"$set": {"a.$": 9}}),
            ({}, {"$set": {"a.$": 9}}),
            ({"n": 7}, {"$set": {"a.$": 9}}),
            ({"_id": 1}, {"$set": {"a.$": 9}}),
            ({"o.k": 2}, {"$set": {"o.$.k": 9}}),
            ({"o.k": 2}, {"$set": {"o.$.new": 9}}),
            ({"o.k": 2}, {"$unset": {"o.$.k": ""}}),
            ({"o.k": 2}, {"$set": {"o.$": 9}}),
            ({"o.k": 2}, {"$unset": {"o.$": ""}}),
            ({"o": {"$elemMatch": {"k": 2, "t": []}}}, {"$set": {"o.$.k": 9}}),
            ({"o.k": 2}, {"$push": {"o.$.t": 9}}),
            ({"o.k": 2}, {"$set": {"o.$.t.$": 9}}),
            ({"o.t": 3}, {"$set": {"o.$.t.0": 9}}),
            ({"a": 2, "b": 6}, {"$set": {"a.$": 9}}),
            ({"b": 6, "a": 2}, {"$set": {"a.$": 9}}),
            ({"a": 2, "b": 6}, {"$set": {"b.$": 9}}),
            ({"a": 2}, {"$set": {"b.$": 9}}),
            ({"a": 3}, {"$set": {"b.$": 9}}),
            ({"a": 2}, {"$set": {"n.$": 9}}),
            ({"a": 2}, {"$set": {"nope.$": 9}}),
            ({"a": 2}, {"$set": {"$": 9}}),
            ({"a": 2}, {"$set": {"$.x": 9}}),
            ({"a": 2}, {"$set": {"a.$.x": 9}}),
            ({"a": {"$ne": 1}}, {"$set": {"a.$": 9}}),
            ({"a": {"$not": {"$gt": 1}}}, {"$set": {"a.$": 9}}),
            ({"$or": [{"a": 3}, {"a": 2}]}, {"$set": {"a.$": 9}}),
            ({"$and": [{"a": 3}, {"n": 7}]}, {"$set": {"a.$": 9}}),
            ({"a": {"$elemMatch": {"$gt": 2}}}, {"$set": {"a.$": 9}}),
            ({"a.1": 2}, {"$set": {"a.$": 9}}),
            ({"nn": [3, 4]}, {"$set": {"nn.$": 9}}),
            ({"a": 2}, {"$rename": {"a.$": "z"}}),
            ({"a": 2}, {"$pull": {"a.$": 2}}),
            ({"a": 9}, {"$set": {"a.$": 1}}),
        ]
    ],
    case(POS, upd({"a": 9}, {"$set": {"a.$": 1}}, upsert=True)),
    case(POS, upd({"a": 2}, {"$set": {"a.$": 9}}, multi=True)),
)
scenario(
    "the all-positional operator",
    *each(
        POS,
        {"$set": {"a.$[]": 9}},
        {"$inc": {"a.$[]": 1}},
        {"$mul": {"b.$[]": 2}},
        {"$set": {"o.$[].k": 9}},
        {"$set": {"o.$[].new": 1}},
        {"$unset": {"o.$[].k": ""}},
        {"$inc": {"o.$[].t.$[]": 10}},
        {"$push": {"o.$[].t": 0}},
        {"$set": {"nn.$[].$[]": 0}},
        {"$set": {"n.$[]": 9}},
        {"$set": {"nope.$[]": 9}},
        {"$set": {"$[]": 9}},
        {"$set": {"a.$[].x": 9}},
        {"$set": {"a.$[]x": 9}},
        {"$unset": {"a.$[]": ""}},
        {"$rename": {"a.$[]": "z"}},
        {"$addToSet": {"o.$[].t": 3}},
        {"$pull": {"o.$[].t": {"$gt": 1}}},
        {"$min": {"a.$[]": 2}},
    ),
    case({"_id": 1, "a": []}, upd({}, {"$set": {"a.$[]": 9}})),
    case({"_id": 1}, upd({"_id": 2}, {"$set": {"a.$[]": 9}}, upsert=True)),
)
scenario(
    "arrayFilters",
    *[
        case(POS, upd({}, u, arrayFilters=f))
        for u, f in [
            ({"$set": {"a.$[x]": 9}}, [{"x": 2}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": {"$gte": 2}}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": {"$in": [1, 3]}}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": 99}]),
            ({"$inc": {"a.$[x]": 1}}, [{"x": {"$gt": 1}}]),
            ({"$set": {"o.$[x].k": 9}}, [{"x.k": 2}]),
            ({"$set": {"o.$[x].k": 9}}, [{"x.k": 2, "x.t": []}]),
            ({"$set": {"o.$[x].k": 9}}, [{"$or": [{"x.k": 1}, {"x.t": 4}]}]),
            ({"$set": {"o.$[x].k": 9}}, [{"$and": [{"x.k": 2}, {"x.t": 3}]}]),
            ({"$set": {"o.$[x].k": 9}}, [{"x": {"k": 2, "t": []}}]),
            ({"$set": {"o.$[x].t.$[y]": 9}}, [{"x.k": {"$gte": 1}}, {"y": {"$gt": 1}}]),
            ({"$set": {"o.$[x].t.$[y]": 9}}, [{"y": {"$gt": 1}}, {"x.k": 2}]),
            ({"$set": {"o.$[].t.$[y]": 9}}, [{"y": {"$gt": 2}}]),
            ({"$set": {"o.$[x].t.$[]": 9}}, [{"x.k": 1}]),
            ({"$set": {"a.$[x]": 9, "b.$[x]": 0}}, [{"x": {"$gt": 2}}]),
            ({"$set": {"a.$[x]": 9, "b.$[y]": 0}}, [{"x": 2}, {"y": 5}]),
            ({"$set": {"a.$[x]": 9}}, []),
            ({"$set": {"a.$[x]": 9}}, [{"y": 2}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": 2}, {"y": 2}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": 2}, {"x": 3}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": 2, "y": 3}]),
            ({"$set": {"a.$[x]": 9}}, [{}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": {"$nope": 1}}]),
            ({"$set": {"a.$[x]": 9}}, [1]),
            ({"$set": {"a.$[x]": 9}}, {"x": 2}),
            ({"$set": {"a.$[x]": 9}}, "x"),
            ({"$set": {"a.$[x]": 9}}, None),
            ({"$set": {"a.$[X]": 9}}, [{"X": 2}]),
            ({"$set": {"a.$[x1]": 9}}, [{"x1": 2}]),
            ({"$set": {"a.$[1x]": 9}}, [{"1x": 2}]),
            ({"$set": {"a.$[x_y]": 9}}, [{"x_y": 2}]),
            ({"$set": {"a.$[x-y]": 9}}, [{"x-y": 2}]),
            ({"$set": {"a.$[ ]": 9}}, [{" ": 2}]),
            ({"$set": {"a": 9}}, [{"x": 2}]),
            ({"$set": {"n.$[x]": 9}}, [{"x": 2}]),
            ({"$set": {"nope.$[x]": 9}}, [{"x": 2}]),
            ({"$set": {"$[x]": 9}}, [{"x": 2}]),
            ({"$set": {"a.$[x].z": 9}}, [{"x": 2}]),
            ({"$unset": {"a.$[x]": ""}}, [{"x": 2}]),
            ({"$pull": {"o.$[x].t": 3}}, [{"x.k": 2}]),
            ({"$push": {"o.$[x].t": {"$each": [0], "$position": 0}}}, [{"x.t": {"$size": 0}}]),
            ({"$set": {"o.$[x].k": 9}}, [{"x.k": {"$exists": True}}]),
            ({"$set": {"o.$[x].k": 9}}, [{"x.nope": {"$exists": False}}]),
            ({"$set": {"o.$[x].k": 9}}, [{"x.t": {"$elemMatch": {"$gt": 3}}}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": {"$type": "int"}}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": {"$mod": [2, 0]}}]),
            ({"$set": {"a.$[x]": 9}}, [{"x": {"$not": {"$eq": 2}}}]),
            ({"a": [0]}, [{"x": 2}]),
            ([{"$set": {"z": 1}}], [{"x": 2}]),
        ]
    ],
    case(POS, upd({"a": 2}, {"$set": {"a.$": 0, "b.$[x]": 0}}, arrayFilters=[{"x": 5}])),
    case(POS, upd({"_id": 5}, {"$set": {"a.$[x]": 9}}, arrayFilters=[{"x": 2}], upsert=True)),
    case(
        POS,
        upd({"_id": 5, "a": [1, 2]}, {"$set": {"a.$[x]": 9}}, arrayFilters=[{"x": 2}], upsert=True),
    ),
)

# ------------------------------------------------------------ pipeline updates

PIPE = {"_id": 1, "a": 5, "b": {"c": 2}, "arr": [1, 2, 3], "s": "x"}

scenario(
    "pipeline updates",
    *each(
        PIPE,
        [],
        [{"$set": {"a": 6}}],
        [{"$set": {"a": 5}}],
        [{"$set": {"t": {"$add": ["$a", "$b.c"]}}}],
        [{"$addFields": {"t": {"$sum": "$arr"}}}],
        [{"$set": {"a": "$$REMOVE"}}],
        [{"$set": {"b.d": 1}}, {"$unset": "b.c"}],
        [{"$unset": ["a", "s"]}],
        [{"$unset": "_id"}],
        [{"$project": {"a": 1}}],
        [{"$project": {"_id": 0, "a": 1}}],
        [{"$project": {"a": 0}}],
        [{"$replaceRoot": {"newRoot": "$b"}}],
        [{"$replaceRoot": {"newRoot": {"_id": 1, "z": "$a"}}}],
        [{"$replaceRoot": {"newRoot": {"_id": 2, "z": "$a"}}}],
        [{"$replaceWith": {"x": "$$ROOT"}}],
        [{"$replaceWith": "$s"}],
        [{"$set": {"_id": 2}}],
        [{"$set": {"_id": 1}}],
        [{"$set": {"_id": 1.0}}],
        [{"$set": {"now": "$$NOW"}}],
        [{"$set": {"ct": "$$CLUSTER_TIME"}}],
        [{"$set": {"a": {"$literal": "$a"}}}],
        [{"$set": {"x": {"$const": 1}}}],
        [{"$match": {"a": 5}}],
        [{"$group": {"_id": None}}],
        [{"$sort": {"a": 1}}],
        [{"$limit": 1}],
        [{"$lookup": {"from": "c", "localField": "a", "foreignField": "a", "as": "j"}}],
        [{"$out": "z"}],
        [{"$nope": 1}],
        [{"$set": {"a": 1}, "$unset": "s"}],
        [{}],
        [1],
        [{"$set": 1}],
        [{"$set": {"a": {"$nope": 1}}}],
        [{"$set": {"a": {"$divide": [1, 0]}}}],
        [{"$set": {"a.b": 1}}],
        [{"$set": {"$a": 1}}],
        [{"$set": {"arr": {"$concatArrays": ["$arr", [4]]}}}],
        [{"$set": {"arr.0": 9}}],
        [{"$set": {"b": {"c": 2}}}],
        [{"$set": {"b": {"d": 3}}}],
        [{"$set": {"x": "$$nope"}}],
        [{"$set": {"x": "$$ROOT.a"}}],
        [{"$set": {"x": "$$CURRENT.s"}}],
        [{"$set": {"big": {"$multiply": [2147483647, 2]}}}],
        [{"$set": {"a": {"$toLong": "$a"}}}],
        [{"$set": {"a": {"$toDouble": "$a"}}}],
    ),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], c={"v": 3})),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], c={})),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], c=1)),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], c={"V": 3})),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], c={"v": "$a"})),
    case(PIPE, upd({}, {"$set": {"x": 1}}, c={"v": 3})),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], top={"let": {"v": 4}})),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], c={"v": 3}, top={"let": {"v": 4}})),
    case(PIPE, upd({"$expr": {"$eq": ["$a", "$$v"]}}, {"$set": {"x": 1}}, top={"let": {"v": 5}})),
    case(PIPE, upd({"$expr": {"$eq": ["$a", "$$v"]}}, {"$set": {"x": 1}}, top={"let": {"v": 6}})),
    case(PIPE, upd({"$expr": {"$eq": ["$a", "$$v"]}}, {"$set": {"x": 1}})),
    *[case(PIPE, upd({}, {"$set": {"x": 1}}, top={"let": v})) for v in [1, "x", [], None, {}]],
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], top={"let": {"v": "$a"}})),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], top={"let": {"v": {"$add": [1, 2]}}})),
    case(PIPE, upd({}, [{"$set": {"x": "$$v"}}], top={"let": {"v": "$$NOW"}})),
    case(PIPE, upd({}, [{"$set": {"x": "$$ROOT"}}], top={"let": {"ROOT": 1}})),
    case(PIPE, upd({}, [{"$set": {"x": "$$1v"}}], top={"let": {"1v": 1}})),
    case([], upd({"_id": 7, "a": 1}, [{"$set": {"b": {"$add": ["$a", 1]}}}], upsert=True)),
    case([], upd({"a": 1}, [{"$replaceWith": {"z": 1}}], upsert=True)),
    case([], upd({"_id": 7}, [{"$set": {"_id": 8}}], upsert=True)),
    case([], upd({"_id": 7}, [{"$unset": "_id"}], upsert=True)),
    case(PIPE, upd({}, [{"$set": {"x": 1}}], multi=True)),
    case(PIPE, upd({}, [{"$set": {"x": 1}}], arrayFilters=[{"x": 1}])),
)

# ----------------------------------------------------------------- replacement

scenario(
    "replacement",
    *each(
        PIPE,
        {},
        {"z": 1},
        {"_id": 1, "z": 1},
        {"_id": 1.0, "z": 1},
        {"_id": Int64(1), "z": 1},
        {"_id": 2, "z": 1},
        {"z": 1, "_id": 1},
        {"a": 5, "b": {"c": 2}, "arr": [1, 2, 3], "s": "x"},
        {"_id": 1, "a": 5, "b": {"c": 2}, "arr": [1, 2, 3], "s": "x"},
        {"s": "x", "arr": [1, 2, 3], "b": {"c": 2}, "a": 5},
        {"a.b": 1},
        {"a": {"b.c": 1}},
        {"a": {"$b": 1}},
        {"": 1},
        {"a": {"": 1}},
        {"z": {"$set": 1}},
        {"_id": {"$a": 1}},
        {"_id": [1]},
        {"_id": None},
        {"a": float("nan")},
        {"x": [{"$y": 1}]},
        {"$db": 1},
        {"$ref": "c", "$id": 1},
        {"z": {"$ref": "c", "$id": 1}},
        {"z": {"$id": 1}},
    ),
    case(PIPE, upd({}, {"z": 1}, multi=True)),
    case(PIPE, upd({"_id": 1}, {"z": 1}, multi=True)),
    case(PIPE, upd({"_id": 3}, {"z": 1}, upsert=True)),
    case(PIPE, upd({"_id": 3}, {"_id": 4, "z": 1}, upsert=True)),
    case(PIPE, upd({"q": 3}, {"z": 1}, upsert=True)),
    case(PIPE, upd({"_id": {"$gt": 5}}, {"z": 1}, upsert=True)),
    case(PIPE, upd({"_id": 1}, {"z": 1}, upsert=True)),
    case(PIPE, upd({"a": 5}, {"_id": 1, "z": 1}, upsert=True)),
)

# --------------------------------------------------------------------- upserts

scenario(
    "upsert: what the query gives the new document",
    *[
        case([], upd(q, {"$set": {"z": 1}}, upsert=True))
        for q in [
            {},
            {"_id": 5},
            {"a": 1},
            {"a": 1, "b": "x"},
            {"a": {"$eq": 1}},
            {"a": {"$gt": 1}},
            {"a": {"$in": [1]}},
            {"a": {"$in": [1, 2]}},
            {"a": {"$all": [1]}},
            {"a": {"$all": [1, 2]}},
            {"a": {"$ne": 1}},
            {"a": {"$exists": True}},
            {"a": None},
            {"a": [1, 2]},
            {"a": {"b": 1}},
            {"a.b": 1},
            {"a.b": 1, "a.c": 2},
            {"a.b.c": {"$eq": [1]}},
            {"a.0": 1},
            {"a": 1, "a.b": 2},
            {"a.b": 2, "a": 1},
            {"$and": [{"a": 1}, {"b": 2}]},
            {"$and": [{"a": 1}, {"a": 1}]},
            {"$and": [{"a": 1}, {"a": 2}]},
            {"$and": [{"a": 1}, {"a": {"$gt": 0}}]},
            {"$or": [{"a": 1}]},
            {"$or": [{"a": 1}, {"b": 2}]},
            {"$nor": [{"a": 1}]},
            {"a": {"$eq": 1, "$gt": 0}},
            {"a": {"$elemMatch": {"b": 1}}},
            {"a": {"$not": {"$gt": 1}}},
            {"a": {"$regex": "x"}},
            {"a": {"$type": "int"}},
            {"a": {"$size": 0}},
            {"a": {"$eq": {"$b": 1}}},
            {"$expr": {"$eq": ["$a", 1]}},
            {"_id": {"$eq": 5}},
            {"_id": {"$in": [5]}},
            {"_id": {"$in": [5, 6]}},
            {"_id": {"$gt": 5}},
            {"_id": {"a": 1, "b": 2}},
            {"_id.a": 1, "_id.b": 2},
            {"_id.a": 1},
            {"_id": [1]},
            {"_id": None},
            {"_id": {"$exists": False}},
            {"_id": 5, "_id.x": 1},
            {"$and": [{"_id": 5}, {"_id": 5}]},
            {"$and": [{"_id": 5}, {"_id": 6}]},
            {"a": {"$all": [{"$elemMatch": {"b": 1}}]}},
            {"$comment": "x", "a": 1},
            {"a": Int64(1), "b": 1.0, "c": Decimal128("1")},
        ]
    ],
)
scenario(
    "upsert: the update on a new document",
    *[
        case([], upd(q, u, upsert=True))
        for q, u in [
            ({"_id": 5}, {"$setOnInsert": {"z": 1}}),
            ({"_id": 5}, {"$setOnInsert": {"z": 1}, "$set": {"y": 2}}),
            ({"_id": 5}, {"$setOnInsert": {"_id": 5}}),
            ({"_id": 5}, {"$setOnInsert": {"_id": 6}}),
            ({"a": 1}, {"$setOnInsert": {"_id": 6}}),
            ({"a": 1}, {"$set": {"_id": 6}}),
            ({"_id": 5}, {"$set": {"_id": 5}}),
            ({"_id": 5}, {"$set": {"_id": 6}}),
            ({"_id": 5}, {"$inc": {"_id": 1}}),
            ({"_id": 5}, {"$unset": {"_id": ""}}),
            ({"a": 1}, {"$inc": {"a": 1}}),
            ({"a": 1}, {"$set": {"a": 2}}),
            ({"a": 1}, {"$unset": {"a": ""}}),
            ({"a": 1}, {"$push": {"a": 2}}),
            ({"a": 1}, {"$push": {"l": 2}}),
            ({"a": 1}, {"$addToSet": {"l": {"$each": [1, 1]}}}),
            ({"a": 1}, {"$pull": {"l": 1}}),
            ({"a": 1}, {"$pop": {"l": 1}}),
            ({"a": 1}, {"$rename": {"a": "b"}}),
            ({"a": 1}, {"$rename": {"x": "y"}}),
            ({"a": 1}, {"$min": {"m": 1}, "$max": {"x": 2}}),
            ({"a": 1}, {"$mul": {"m": 2}}),
            ({"a": 1}, {"$bit": {"m": {"or": 2}}}),
            ({"a": 1}, {"$currentDate": {"t": True}}),
            ({"a.b": 1}, {"$set": {"a.c": 2}}),
            ({"a.b": 1}, {"$set": {"a": 2}}),
            ({"a": 1}, {"$set": {"a.b": 2}}),
            ({"a": {"b": 1}}, {"$set": {"a.c": 2}}),
            ({"a": 1}, {"$set": {"l.$": 2}}),
            ({"l": 1}, {"$set": {"l.$": 2}}),
            ({"a": 1}, {"$set": {"l.$[]": 2}}),
            ({"a": 1}, {}),
            ({"a": 1}, {"$set": {}}),
            ({"a": 1}, {"$setOnInsert": {}}),
            ({"a": 1}, {"$setOnInsert": {"a.b": 1}}),
            ({"a": 1}, {"$setOnInsert": {"$x": 1}}),
            ({"a": 1}, {"$setOnInsert": {"x": {"$y": 1}}}),
            ({"_id": {"x": 1}}, {"$set": {"_id.y": 2}}),
            ({"_id": 5}, {"$set": {"_id.y": 2}}),
            ({"_id.x": 1}, {"$set": {"z": 2}}),
        ]
    ],
)
scenario(
    "upsert: the reply",
    {
        "update": "c",
        "updates": [
            {"q": {"_id": 1}, "u": {"$set": {"a": 1}}, "upsert": True},
            {"q": {"_id": 1}, "u": {"$set": {"a": 1}}, "upsert": True},
            {"q": {"_id": 1}, "u": {"$set": {"a": 2}}, "upsert": True},
            {"q": {"a": 9}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"_id": "s"}, "u": {"x": 1}, "upsert": True},
            {"q": {"_id": {"k": 1}}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"_id": Int64(7)}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"_id": 8.5}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"_id": DATE}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"_id": 99}, "u": {"$set": {"b": 1}}},
        ],
    },
    {"find": "c", "filter": {"_id": {"$type": "number"}}, "sort": {"_id": 1}},
    {
        "createIndexes": "c",
        "indexes": [{"key": {"u": 1}, "name": "u_1", "unique": True, "sparse": True}],
    },
    {
        "update": "c",
        "updates": [
            {"q": {"u": 1}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"u": 1, "nope": 1}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"_id": 500}, "u": {"$set": {"b": 1}}, "upsert": True},
        ],
    },
    {
        "update": "c",
        "updates": [
            {"q": {"u": 1, "nope": 1}, "u": {"$set": {"b": 1}}, "upsert": True},
            {"q": {"_id": 501}, "u": {"$set": {"b": 1}}, "upsert": True},
        ],
        "ordered": False,
    },
    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"u": 1}}}]},
    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"_id": 1, "u": 1}}]},
    {"update": "c", "updates": [{"q": {}, "u": {"$set": {"u": 3}}, "multi": True}]},
    {"find": "c", "filter": {"_id": {"$type": "number"}}, "sort": {"_id": 1}},
    {"update": "c", "updates": [{"q": {"_id": 600}, "u": {"$set": {"b": 1}}, "upsert": True, "multi": True}]},
    {"update": "c", "updates": [{"q": {"zz": 1}, "u": {"zz": 2}, "upsert": True, "multi": True}]},
)

# ------------------------------------------------------- n, nModified and _id

FIVE = [{"_id": i, "g": i % 2, "v": i} for i in range(5)]

scenario(
    "what counts as a change",
    *[
        case(FIVE, upd(q, u, **more))
        for q, u, more in [
            ({}, {"$set": {"v": 0}}, {}),
            ({}, {"$set": {"v": 0}}, {"multi": True}),
            ({"g": 1}, {"$inc": {"v": 0}}, {"multi": True}),
            ({"g": 1}, {"$inc": {"v": 1}}, {"multi": True}),
            ({"g": 9}, {"$inc": {"v": 1}}, {"multi": True}),
            ({}, {"$unset": {"nope": ""}}, {"multi": True}),
            ({}, {"$set": {"g": 0}}, {"multi": True}),
            ({}, {"$max": {"v": 2}}, {"multi": True}),
            ({}, {"$set": {"v": 0.0}}, {}),
            ({}, {"$set": {"v": Int64(0)}}, {}),
            ({}, {"$set": {"v": -0.0}}, {}),
            ({}, {"$mul": {"v": 1}}, {"multi": True}),
            ({}, {"$mul": {"v": 1.0}}, {"multi": True}),
            ({}, {"$rename": {"nope": "x"}}, {"multi": True}),
            ({}, {"$pull": {"v": 1}}, {}),
            ({}, {"$addToSet": {"l": 1}}, {"multi": True}),
            ({"_id": 0}, {"_id": 0, "g": 0, "v": 0}, {}),
            ({"_id": 0}, {"g": 0, "v": 0}, {}),
            ({"_id": 0}, {"v": 0, "g": 0}, {}),
            ({"_id": 0}, [{"$set": {"v": 0}}], {}),
            ({"_id": 0}, [{"$set": {"v": {"$add": ["$v", 0]}}}], {}),
            ({"_id": 0}, [{"$replaceWith": {"_id": 0, "v": 0, "g": 0}}], {}),
            ({"v": {"$gte": 3}}, {"$set": {"v": 0}}, {"multi": True}),
            ({"v": {"$gte": 0}}, {"$inc": {"v": 10}}, {"multi": True}),
            ({"_id": {"$in": [1, 3, 9]}}, {"$set": {"x": 1}}, {"multi": True}),
            ({"_id": {"$in": [1, 3, 9]}}, {"$set": {"x": 1}}, {}),
        ]
    ],
    case(
        FIVE,
        {
            "update": "c",
            "updates": [
                {"q": {"_id": 0}, "u": {"$set": {"x": 1}}},
                {"q": {"_id": 1}, "u": {"$inc": {"v": "a"}}},
                {"q": {"_id": 2}, "u": {"$set": {"x": 1}}},
                {"q": {"_id": 3}, "u": {"$nope": {"x": 1}}},
                {"q": {"_id": 4}, "u": {"$set": {"x": 1}}},
            ],
        },
    ),
    case(
        FIVE,
        {
            "update": "c",
            "updates": [
                {"q": {"_id": 0}, "u": {"$set": {"x": 1}}},
                {"q": {"_id": 1}, "u": {"$inc": {"v": "a"}}},
                {"q": {"_id": 2}, "u": {"$set": {"x": 1}}},
                {"q": {"_id": 3}, "u": {"$set": {"_id": 9}}},
                {"q": {"_id": 4}, "u": {"$set": {"x": 1}}},
            ],
            "ordered": False,
        },
    ),
    case(
        FIVE,
        {
            "update": "c",
            "updates": [
                {"q": {"_id": 0}, "u": {"$set": {"x": 1}}},
                {"q": {"a": {"$nope": 1}}, "u": {"$set": {"x": 1}}},
                {"q": {"_id": 2}, "u": {"$set": {"x": 1}}},
            ],
            "ordered": False,
        },
    ),
    case(
        [{"_id": i, "v": "s" if i == 2 else i} for i in range(5)],
        upd({}, {"$inc": {"v": 1}}, multi=True),
    ),
)
scenario(
    "the _id is fixed",
    *each(
        {"_id": 1, "a": 1},
        {"$set": {"_id": 1}},
        {"$set": {"_id": 1.0}},
        {"$set": {"_id": Int64(1)}},
        {"$set": {"_id": 2}},
        {"$set": {"_id": "1"}},
        {"$inc": {"_id": 0}},
        {"$inc": {"_id": 1}},
        {"$unset": {"_id": ""}},
        {"$rename": {"_id": "b"}},
        {"$rename": {"a": "_id"}},
        {"$set": {"_id.x": 1}},
        {"$push": {"_id": 1}},
        {"$max": {"_id": 0}},
        {"$max": {"_id": 5}},
        {"$currentDate": {"_id": True}},
        {"$setOnInsert": {"_id": 2}},
        {"$mul": {"_id": 1}},
    ),
    *each(
        {"_id": {"x": 1, "y": 2}, "a": 1},
        {"$set": {"_id.x": 1}},
        {"$set": {"_id.x": 2}},
        {"$set": {"_id.z": 2}},
        {"$unset": {"_id.x": ""}},
        {"$set": {"_id": {"x": 1, "y": 2}}},
        {"$set": {"_id": {"y": 2, "x": 1}}},
        {"_id": {"y": 2, "x": 1}, "a": 1},
    ),
)

# ------------------------------------------------------- the update command

scenario(
    "update: the command",
    {"insert": "c", "documents": FIVE},
    *[{"update": v, "updates": [{"q": {}, "u": {"$set": {"x": 1}}}]} for v in [1, None, "", {}, [], "a.b", "$x", "system.views"]],
    *[{"update": "c", "updates": v} for v in [1, "x", None, {}, [], [1], [{}], [[]]]],
    {"update": "c"},
    *[
        {"update": "c", "updates": [s]}
        for s in [
            {"q": {}},
            {"u": {"$set": {"x": 1}}},
            {"q": 1, "u": {"$set": {"x": 1}}},
            {"q": None, "u": {"$set": {"x": 1}}},
            {"q": [], "u": {"$set": {"x": 1}}},
            {"q": "x", "u": {"$set": {"x": 1}}},
            {"q": {}, "u": 1},
            {"q": {}, "u": "x"},
            {"q": {}, "u": None},
            {"q": {}, "u": True},
            {"q": {}, "u": {"$set": {"x": 1}}, "nope": 1},
            {"q": {}, "u": {"$set": {"x": 1}}, "Multi": True},
            {"q": {}, "u": {"$set": {"x": 1}}, "limit": 1},
            {"q": {"$nope": 1}, "u": {"$set": {"x": 1}}},
            {"q": {"a": {"$nope": 1}}, "u": {"$set": {"x": 1}}},
            {"q": {"$where": "true"}, "u": {"$set": {"x": 1}}},
            {"q": {"$text": {"$search": "x"}}, "u": {"$set": {"x": 1}}},
        ]
    ],
    *[{"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"x": 1}}, "multi": v}]} for v in BOOLS],
    *[{"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"x": 1}}, "upsert": v}]} for v in BOOLS],
    *[{"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"x": 1}}}], "ordered": v} for v in BOOLS],
    *[
        {"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"x": 1}}}], "bypassDocumentValidation": v}
        for v in BOOLS
    ],
    *[
        {"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"x": 1}}, "hint": v}]}
        for v in ["_id_", {"_id": 1}, "nope", {"nope": 1}, {"$natural": 1}, {"$natural": -1}, "$natural", 1, None, [], {}, ""]
    ],
    *[
        {"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"x": 1}}, "collation": v}]}
        for v in [{"locale": "en"}, {"locale": "simple"}, {"locale": "xx"}, {}, 1, "en", None, {"locale": "en", "strength": 9}, {"strength": 1}]
    ],
    *[
        {"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"x": 1}}}], f: v}
        for f, v in [
            ("nope", 1),
            ("comment", "x"),
            ("comment", {"a": 1}),
            ("maxTimeMS", 1000),
            ("maxTimeMS", -1),
            ("maxTimeMS", "x"),
            ("writeConcern", {"w": 1}),
            ("writeConcern", {"w": 0}),
            ("writeConcern", {"w": "majority", "j": True}),
            ("writeConcern", 1),
            ("writeConcern", {"w": -1}),
            ("writeConcern", {"nope": 1}),
            ("readConcern", {"level": "local"}),
            ("collation", {"locale": "en"}),
            ("hint", "_id_"),
            ("arrayFilters", []),
            ("multi", True),
            ("upsert", True),
            ("stmtId", 1),
            ("isTimeseriesNamespace", True),
            ("encryptionInformation", {}),
            ("bypassEmptyTsReplacement", True),
        ]
    ],
    {"update": "nope", "updates": [{"q": {}, "u": {"$set": {"x": 1}}}]},
    {"update": "nope", "updates": [{"q": {}, "u": {"$set": {"x": 1}}, "multi": True}]},
    {"update": "nope", "updates": [{"q": {}, "u": {"$nope": {"x": 1}}}]},
    {"update": "nope", "updates": [{"q": {"a": {"$nope": 1}}, "u": {"$set": {"x": 1}}}]},
    {"listCollections": 1, "nameOnly": True},
    {"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"s": "a"}}}], "let": {"v": 1}},
    {"update": "c", "updates": [{"q": {"s": "A"}, "u": {"$set": {"hit": 1}}, "collation": {"locale": "en", "strength": 2}}]},
    {"update": "c", "updates": [{"q": {"s": "A"}, "u": {"$set": {"hit": 2}}}]},
    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"l": ["a", "B"]}}}]},
    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$pull": {"l": "b"}}, "collation": {"locale": "en", "strength": 2}}]},
    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$addToSet": {"l": "A"}}, "collation": {"locale": "en", "strength": 2}}]},
    {
        "update": "c",
        "updates": [{"q": {"_id": 1}, "u": {"$set": {"l.$[e]": "z"}}, "arrayFilters": [{"e": "A"}], "collation": {"locale": "en", "strength": 2}}],
    },
    {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$max": {"s": "B"}}, "collation": {"locale": "en", "strength": 2}}]},
    FIND,
)

# -------------------------------------------------------------- findAndModify

scenario(
    "findAndModify: what it returns",
    *[
        case(FIVE, fam(**f))
        for f in [
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "new": True},
            {"query": {"_id": 9}, "update": {"$set": {"x": 1}}},
            {"query": {"_id": 9}, "update": {"$set": {"x": 1}}, "new": True},
            {"query": {"_id": 9}, "update": {"$set": {"x": 1}}, "upsert": True},
            {"query": {"_id": 9}, "update": {"$set": {"x": 1}}, "upsert": True, "new": True},
            {"query": {"q": 9}, "update": {"$set": {"x": 1}}, "upsert": True, "new": True},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "upsert": True},
            {"query": {"_id": 1}, "update": {"$set": {"v": 1}}},
            {"query": {"_id": 1}, "update": {"$set": {"v": 1}}, "new": True},
            {"query": {"_id": 1}, "update": {"z": 1}},
            {"query": {"_id": 1}, "update": {"z": 1}, "new": True},
            {"query": {"_id": 1}, "update": {"_id": 2, "z": 1}},
            {"query": {"_id": 1}, "update": [{"$set": {"z": "$v"}}], "new": True},
            {"query": {"_id": 1}, "update": []},
            {"query": {"_id": 1}, "update": {}},
            {"query": {"_id": 1}, "remove": True},
            {"query": {"_id": 9}, "remove": True},
            {"query": {}, "remove": True},
            {"query": {}, "remove": True, "sort": {"v": -1}},
            {"query": {"g": 1}, "sort": {"v": -1}, "update": {"$set": {"x": 1}}},
            {"query": {"g": 1}, "sort": {"v": 1}, "update": {"$set": {"x": 1}}, "new": True},
            {"query": {}, "sort": {"g": -1, "v": -1}, "update": {"$inc": {"v": 100}}, "new": True},
            {"query": {}, "sort": {"$natural": -1}, "update": {"$set": {"x": 1}}},
            {"query": {}, "sort": {"nope": 1}, "update": {"$set": {"x": 1}}},
            {"update": {"$set": {"x": 1}}},
            {"remove": True},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "fields": {"v": 1}},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "fields": {"v": 1}, "new": True},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "fields": {"x": 1, "_id": 0}, "new": True},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "fields": {"v": 0}},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "fields": {}},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "fields": {"v": 1, "g": 0}},
            {"query": {"_id": 1}, "update": {"$set": {"x": 1}}, "fields": {"t": {"$add": ["$v", 1]}}},
            {"query": {"_id": 1}, "remove": True, "fields": {"v": 1}},
            {"query": {"_id": 9}, "update": {"$set": {"x": 1}}, "upsert": True, "new": True, "fields": {"x": 1}},
            {"query": {"_id": 1}, "update": {"$inc": {"v": "a"}}},
            {"query": {"_id": 1}, "update": {"$set": {"_id": 5}}},
            {"query": {"_id": 1}, "update": {"$nope": {"x": 1}}},
            {"query": {"a": {"$nope": 1}}, "update": {"$set": {"x": 1}}},
            {"query": {"_id": 2}, "update": {"_id": 2, "g": 0, "v": 2}},
            {"query": {"_id": 2}, "update": {"$set": {"l": [1, 2, 3]}}, "new": True},
        ]
    ],
    case(
        {"_id": 1, "l": [1, 2, 3]},
        fam(query={"l": 2}, update={"$set": {"l.$": 9}}, new=True),
    ),
    case(
        {"_id": 1, "l": [1, 2, 3]},
        fam(query={"l": 2}, update={"$inc": {"l.$": 0}}, fields={"l.$": 1}),
    ),
    case(
        {"_id": 1, "l": [1, 2, 3]},
        fam(query={}, update={"$set": {"l.$[e]": 9}}, arrayFilters=[{"e": {"$gt": 1}}], new=True),
    ),
    case({"_id": 1, "l": [1, 2, 3]}, fam(query={}, update={"$set": {"l.$[e]": 9}})),
    case({"_id": 1, "l": [1, 2, 3]}, fam(query={}, remove=True, arrayFilters=[{"e": 1}])),
    case({"_id": 1, "l": [1, 2, 3]}, fam(query={}, update={"$set": {"x": 1}}, arrayFilters=[{"e": 1}])),
    case({"_id": 1, "l": [1, 2, 3]}, fam(query={}, update=[{"$set": {"x": 1}}], arrayFilters=[{"e": 1}])),
    case({"_id": 1, "l": [1, 2, 3]}, fam(query={}, update={"$set": {"x": 1}}, arrayFilters=[])),
    case({"_id": 1, "l": [1, 2, 3]}, fam(query={}, update={"$set": {"x": 1}}, arrayFilters=1)),
    case(FIVE, {"findandmodify": "c", "query": {"_id": 1}, "update": {"$set": {"x": 1}}}),
    case(FIVE, {"findAndModify": "nope", "query": {"_id": 1}, "update": {"$set": {"x": 1}}}),
    case(FIVE, {"findAndModify": "nope", "query": {"_id": 1}, "remove": True}),
    case(FIVE, {"findAndModify": "nope", "query": {"_id": 1}, "update": {"$set": {"x": 1}}, "upsert": True}),
)
scenario(
    "findAndModify: the arguments",
    {"insert": "c", "documents": FIVE},
    *[{"findAndModify": v, "query": {}, "update": {"$set": {"x": 1}}} for v in [1, None, "", {}, [], "a.b", "$x", "system.views"]],
    fam(),
    fam(query={}),
    fam(query={}, update={"$set": {"x": 1}}, remove=True),
    fam(query={}, remove=True, new=True),
    fam(query={}, remove=True, upsert=True),
    fam(query={}, remove=True, new=False),
    fam(query={}, remove=False),
    fam(query={}, remove=False, update={"$set": {"x": 1}}),
    fam(query={"_id": 99}, remove=True, upsert=False),
    *[fam(query=v, update={"$set": {"x": 1}}) for v in [1, "x", None, [], True]],
    *[fam(query={"_id": 0}, update=v) for v in [1, "x", None, True, [1], [{"$nope": 1}]]],
    *[fam(query={"_id": 99}, remove=v) for v in BOOLS],
    *[fam(query={"_id": 0}, update={"$set": {"x": 1}}, new=v) for v in BOOLS],
    *[fam(query={"_id": 99}, update={"$set": {"x": 1}}, upsert=v) for v in [1, 0, "x", None, {}, 2.5]],
    *[fam(query={"_id": 0}, update={"$set": {"x": 1}}, sort=v) for v in [1, "x", None, [], {}, {"v": 0}, {"v": 2}, {"v": "x"}, {"v": {"$meta": "textScore"}}, {"": 1}]],
    *[fam(query={"_id": 0}, update={"$set": {"x": 1}}, fields=v) for v in [1, "x", None, [], {"v": "x"}, {"v": 2}, {"$v": 1}, {"v": {"$nope": 1}}, {"l": {"$slice": 1}}, {"l": {"$elemMatch": {"a": 1}}}]],
    *[fam(query={"_id": 0}, update={"$set": {"x": 1}}, bypassDocumentValidation=v) for v in BOOLS],
    *[
        fam(query={"_id": 0}, update={"$set": {"x": 1}}, hint=v)
        for v in ["_id_", {"_id": 1}, "nope", {"nope": 1}, {"$natural": 1}, "$natural", 1, None, [], {}]
    ],
    *[
        fam(query={"_id": 0}, update={"$set": {"x": 1}}, collation=v)
        for v in [{"locale": "en"}, {"locale": "xx"}, {}, 1, "en", None]
    ],
    *[fam(query={"_id": 0}, update={"$set": {"x": 1}}, let=v) for v in [{"v": 1}, {}, 1, "x", None, []]],
    fam(query={"$expr": {"$eq": ["$_id", "$$v"]}}, update=[{"$set": {"y": "$$v"}}], let={"v": 3}, new=True),
    fam(query={"$expr": {"$eq": ["$_id", "$$v"]}}, remove=True, let={"v": 3}),
    fam(query={"$expr": {"$eq": ["$_id", "$$v"]}}, update={"$set": {"x": 1}}),
    *[
        fam(query={"_id": 0}, update={"$set": {"x": 1}}, **{f: v})
        for f, v in [
            ("nope", 1),
            ("comment", "x"),
            ("maxTimeMS", 1000),
            ("maxTimeMS", -1),
            ("maxTimeMS", "x"),
            ("writeConcern", {"w": 1}),
            ("writeConcern", {"w": 0}),
            ("writeConcern", 1),
            ("writeConcern", {"w": -1}),
            ("readConcern", {"level": "local"}),
            ("multi", True),
            ("ordered", True),
            ("limit", 1),
            ("skip", 1),
            ("stmtId", 1),
            ("bypassEmptyTsReplacement", True),
            ("batchSize", 1),
            ("projection", {"v": 1}),
            ("filter", {"_id": 1}),
        ]
    ],
    FIND,
    {"createIndexes": "c", "indexes": [{"key": {"u": 1}, "name": "u_1", "unique": True, "sparse": True}]},
    fam(query={"_id": 0}, update={"$set": {"u": 1}}),
    fam(query={"_id": 1}, update={"$set": {"u": 1}}),
    fam(query={"_id": 50}, update={"$set": {"u": 1}}, upsert=True),
    fam(query={"_id": 1}, update={"_id": 1, "u": 1}),
    {"create": "v", "viewOn": "c", "pipeline": []},
    {"findAndModify": "v", "query": {}, "update": {"$set": {"x": 1}}},
    {"findAndModify": "v", "query": {}, "remove": True},
    {"update": "v", "updates": [{"q": {}, "u": {"$set": {"x": 1}}}]},
    {"update": "system.views", "updates": [{"q": {}, "u": {"$set": {"x": 1}}}]},
    {"findAndModify": "system.views", "query": {}, "remove": True},
    FIND,
)

# ------------------------------------------------------------ what is stored

scenario(
    "what a write may store",
    *each(
        {"_id": 1},
        {"$set": {"a": {"b": {"c": {"d": {"e": {"f": {"g": 1}}}}}}}},
        {"$set": {"a.b.c.d.e.f.g.h.i.j.k.l.m": 1}},
        *[{"$set": {".".join(["a"] * n): 1}} for n in (100, 178, 179, 180, 181, 199)],
        *[{"$set": {".".join(["a"] * n): {"b": {"c": 1}}}} for n in (176, 177, 178, 179)],
        *[{"$set": {".".join(["a"] * n): [[1]]}} for n in (177, 178, 179)],
        *[{"$push": {".".join(["a"] * n): {"b": 1}}} for n in (177, 178, 179)],
        {"$set": {"s": "x" * 1000}},
        {"$set": {"a": MinKey(), "b": MaxKey()}},
        {"$set": {"a": Timestamp(0, 0)}},
        {"$set": {"a": Timestamp(5, 6)}},
        {"$set": {"a": DATE}},
        {"$set": {"a": ObjectId("0123456789abcdef01234567")}},
        {"$set": {"a": "x\x00y"}},
        {"$set": {"ünï.cödé": "é"}},
        {"$set": {"a": float("inf"), "b": float("-inf"), "c": float("nan")}},
        {"$set": {"a": Decimal128("NaN"), "b": Decimal128("-0"), "c": Decimal128("1E+6144")}},
        {"$set": {"a": [[[[[1]]]]]}},
        {"$set": {"a": {"$numberLong": "1"}}},
        {"$set": {"a": {"x": 1, "x2": 1}}},
    ),
    case({"_id": 1, "a": Timestamp(0, 0)}, upd({}, {"$set": {"b": Timestamp(0, 0)}})),
    case([], {"insert": "c", "documents": [{"_id": 1, "a": Timestamp(0, 0), "b": {"t": Timestamp(0, 0)}}]}),
    case([], upd({"_id": 1}, {"a": Timestamp(0, 0), "b": {"t": Timestamp(0, 0)}}, upsert=True)),
    case([], upd({"_id": 1}, {"$set": {"a": Timestamp(0, 0)}}, upsert=True)),
    case({"_id": 1, "a": 1}, upd({}, {"t": Timestamp(0, 0)})),
)

scenario(
    "a sub-document that looks like extended JSON",
    *[
        step
        for shape in [
            {"$numberLong": "1"},
            {"$numberInt": "1"},
            {"$numberDouble": "1.5"},
            {"$numberDecimal": "1.5"},
            {"$oid": "0123456789abcdef01234567"},
            {"$date": 5},
            {"$symbol": "s"},
            {"$code": "f()"},
            {"$timestamp": {"t": 1, "i": 2}},
            {"$regularExpression": {"pattern": "a", "options": "i"}},
            {"$binary": {"base64": "AA==", "subType": "00"}},
            {"$uuid": "00000000-0000-0000-0000-000000000000"},
            {"$minKey": 1},
            {"$maxKey": 1},
            {"$undefined": True},
        ]
        for step in [
            # Stored by an insert, then another field of the document is written.
            case({"_id": 1, "a": shape, "l": [shape]}, upd({}, {"$set": {"z": 1}})),
            case({"_id": 1, "a": shape}, upd({}, [{"$set": {"z": 1}}])),
            case({"_id": 1, "a": shape}, fam(query={}, update={"$inc": {"n": 1}}, new=True)),
            # Written by the update itself.
            case({"_id": 1}, upd({}, {"$set": {"a": shape}})),
            case({"_id": 1}, upd({}, {"$push": {"l": shape}})),
            case({"_id": 1}, upd({}, {"a": shape})),
            case([], upd({"_id": 1}, {"$set": {"a": shape}}, upsert=True)),
        ]
    ],
    case(
        {"_id": 1, "a": {"$numberLong": "1"}},
        {"aggregate": "c", "pipeline": [{"$addFields": {"q": 1}}], "cursor": {}},
    ),
    case({"_id": 1, "a": {"$numberLong": "1"}}, {"distinct": "c", "key": "a"}),
    case({"_id": 1, "a": {"$numberLong": "1"}}, {"find": "c", "filter": {"a": 1}}),
    case({"_id": 1, "a": {"$numberLong": "1"}}, {"find": "c", "filter": {"a.$numberLong": "1"}}),
    case({"_id": 1, "a": {"$numberLong": "1"}}, {"count": "c", "query": {"a": {"$type": "object"}}}),
    case(
        {"_id": 1, "a": {"$numberLong": "1"}},
        {"delete": "c", "deletes": [{"q": {"a": {"$type": "long"}}, "limit": 0}]},
    ),
)

TWO = {"_id": 1, "a": [1, 2, 3, 2], "b": [5, 6, 7, 8], "o": [{"k": 1}, {"k": 2}]}

scenario(
    "the positional operator: which element",
    *[
        case(TWO, upd(q, u))
        for q, u in [
            ({"a": 3, "b": 5}, {"$set": {"a.$": 9}}),
            ({"a": 3, "b": 5}, {"$set": {"b.$": 9}}),
            ({"b": 5, "a": 3}, {"$set": {"a.$": 9}}),
            ({"b": 8, "a": 1}, {"$set": {"a.$": 9}}),
            ({"b": 8, "a": 1}, {"$set": {"b.$": 9}}),
            ({"a": 3, "o.k": 2}, {"$set": {"a.$": 9}}),
            ({"o.k": 2, "a": 3}, {"$set": {"o.$.k": 9}}),
            ({"a": {"$in": [3, 2]}}, {"$set": {"a.$": 9}}),
            ({"$or": [{"a": 3}, {"b": 6}]}, {"$set": {"a.$": 9}}),
            ({"$or": [{"a": 99}, {"b": 8}]}, {"$set": {"a.$": 9}}),
            ({"$or": [{"a": 3}, {"a": 2}]}, {"$set": {"a.$": 9}}),
            ({"a": {"$gt": 1}, "b": {"$gt": 7}}, {"$set": {"a.$": 9}}),
            ({"a": 2, "_id": 1}, {"$set": {"x.$": 9}}),
            ({"a.1": 2}, {"$set": {"a.$": 9}}),
            ({"a": {"$elemMatch": {"$gt": 2}}, "b": 6}, {"$set": {"a.$": 9}}),
            ({"a": {"$not": {"$gt": 5}}}, {"$set": {"a.$": 9}}),
            ({"a": {"$size": 4}}, {"$set": {"a.$": 9}}),
            ({"a": {"$all": [3, 2]}}, {"$set": {"a.$": 9}}),
            ({"a": {"$exists": True}}, {"$set": {"a.$": 9}}),
            ({"a": [1, 2, 3, 2]}, {"$set": {"a.$": 9}}),
            ({"a": {"$type": "int"}}, {"$set": {"a.$": 9}}),
            ({"a": {"$mod": [3, 0]}}, {"$set": {"a.$": 9}}),
            ({"a": {"$gt": 1, "$lt": 3}}, {"$set": {"a.$": 9}}),
            ({"a": {"$lt": 3, "$gt": 1}}, {"$set": {"a.$": 9}}),
            ({"a": {"$gte": 2, "$lte": 3}}, {"$set": {"a.$": 9}}),
            ({"$nor": [{"a": 9}]}, {"$set": {"a.$": 9}}),
            ({"o": {"k": 2}}, {"$set": {"o.$.k": 9}}),
            ({"$and": [{"a": 2}, {"a": 3}]}, {"$set": {"a.$": 9}}),
            ({"$and": [{"a": 3}, {"a": 2}]}, {"$set": {"a.$": 9}}),
            ({"a": {"$regex": "x"}}, {"$set": {"a.$": 9}}),
            ({"a": {"$bitsAllSet": 2}}, {"$set": {"a.$": 9}}),
            ({"a": {"$eq": 3}, "b": {"$in": [7, 6]}}, {"$set": {"a.$": 9}}),
            ({"a": {"$in": [3]}, "b": 6}, {"$set": {"a.$": 9}}),
            ({"a": 3}, {"$set": {"a.$": 9, "b.$": 0}}),
            ({"a": 3}, {"$inc": {"b.$": 1}, "$set": {"o.$": 0}}),
        ]
    ],
    case(TWO, fam(query={"a": 3, "b": 5}, update={"$set": {"a.$": 9}}, new=True)),
    case(TWO, fam(query={"a": 3}, update={"$set": {"b.$": 9}}, new=True)),
    case(TWO, fam(query={"o.k": 2}, update={"$set": {"o.$.k": 9}}, new=True, fields={"o.$": 1})),
)
scenario(
    "$push with $sort: what counts as a change",
    *each(
        {"_id": 1, "e": [], "one": [1], "s": [1, 2, 3], "u": [3, 1], "o": [{"k": 1}, {"k": 2}]},
        {"$push": {"e": {"$each": [], "$sort": 1}}},
        {"$push": {"one": {"$each": [], "$sort": 1}}},
        {"$push": {"s": {"$each": [], "$sort": 1}}},
        {"$push": {"u": {"$each": [], "$sort": 1}}},
        {"$push": {"o": {"$each": [], "$sort": {"k": 1}}}},
        {"$push": {"s": {"$each": [], "$slice": 5}}},
        {"$push": {"s": {"$each": []}}},
        {"$push": {"s": {"$each": [], "$position": 0}}},
        {"$push": {"nope": {"$each": [], "$sort": 1}}},
        {"$push": {"nope": {"$each": []}}},
        {"$push": {"s": {"$each": [], "$sort": 1, "$slice": 5}}},
        {"$push": {"s": {"$each": [], "$slice": -5}}},
        {"$addToSet": {"nope": {"$each": []}}},
        {"$pull": {"nope": 1}},
        {"$pullAll": {"s": []}},
    ),
)
scenario(
    "a field an operator may not create",
    *[
        case({"_id": 1, "o": {}, "s": 1}, upd(q, u, **more))
        for q, u, more in [
            ({"_id": 5}, {"$set": {"$x": 1}}, {"upsert": True}),
            ({"_id": 5}, {"$setOnInsert": {"$x": 1}}, {"upsert": True}),
            ({"_id": 5}, {"$set": {"a.$x": 1}}, {"upsert": True}),
            ({}, {"$set": {"o.$x": 1}}, {}),
            ({}, {"$set": {"$x.y": 1}}, {}),
            ({}, {"$inc": {"$x": 1}}, {}),
            ({}, {"$push": {"$x": 1}}, {}),
            ({}, {"$unset": {"$x": 1}}, {}),
            ({}, {"$min": {"$x": 1}}, {}),
            ({}, {"$rename": {"s": "o.$z"}}, {}),
            ({}, {"$currentDate": {"$x": True}}, {}),
            ({}, {"$set": {"$id": 1}}, {}),
            ({}, {"$set": {"$ref": 1}}, {}),
            ({}, {"$set": {"$db": 1}}, {}),
            ({}, {"$set": {"$ref": "c"}}, {}),
            ({}, {"$set": {"$ref": "c", "$id": 1}}, {}),
            ({}, {"$set": {"$ref": "c", "$id": 1, "$db": "d"}}, {}),
            ({}, {"$set": {"$id": 1, "$ref": "c"}}, {}),
            ({}, {"$set": {"$db": "d"}}, {}),
        ]
    ],
    case({"_id": 1, "$x": 1}, upd({}, {"$set": {"z": 1}})),
    case({"_id": 1, "$x": 1}, upd({}, {"$inc": {"$x": 1}})),
    case({"_id": 1, "$x": 1}, upd({}, {"$set": {"$y": 1}})),
)

# ------------------------------------------------------------------ running

UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

OID_TEXT = re.compile(r"ObjectId\('[0-9a-f]{24}'\)")
SOON = datetime.timedelta(days=1)


def clean(value, db, key=None):
    if isinstance(value, dict):
        return {k: clean(v, db, k) for k, v in value.items() if k not in DROP}
    if isinstance(value, list):
        return [clean(v, db, key) for v in value]
    if isinstance(value, ObjectId):
        return value if str(value) == "0123456789abcdef01234567" else "OID"
    if isinstance(value, datetime.datetime):
        now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
        return "NOW" if abs(now - value.replace(tzinfo=None)) < SOON else value
    if isinstance(value, Timestamp):
        now = datetime.datetime.now(datetime.timezone.utc).timestamp()
        return "TIMESTAMP-NOW" if abs(now - value.time) < 86400 else value
    if isinstance(value, Int64):
        return f"Int64({int(value)})"
    if isinstance(value, float):
        if value != value:
            return "NaN"
        return f"double({value!r})"
    if isinstance(value, str):
        text = OID_TEXT.sub("OID", value.replace(db, "@db"))
        if "Index build failed" in text:
            text = UUID_TEXT.sub("UUID", text)
        return re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            text,
        )
    return value


def send(wire, uri, db, command):
    """One command; a connection that fails is replaced and reported."""
    try:
        return wire, wire.command(db, command)
    except TimeoutError:
        wire.close()
        return Wire(uri), {"probe": "TIMEOUT"}
    except (ConnectionError, OSError) as exc:
        wire.close()
        return Wire(uri), {"probe": f"CONNECTION {type(exc).__name__}"}


def run_scenario(uri, index, steps):
    db = f"updp{RUN}_{index}"
    wire = Wire(uri)
    replies = []
    for step in steps:
        if isinstance(step, tuple):
            _, seed, command = step
            wire, _ = send(wire, uri, db, {"drop": "c"})
            if seed:
                wire, made = send(wire, uri, db, {"insert": "c", "documents": seed})
                if made.get("n") != len(seed):
                    replies.append({"probe": "SEED", "reply": clean(made, db)})
                    continue
            wire, reply = send(wire, uri, db, command)
            wire, after = send(wire, uri, db, FIND)
            docs = after.get("cursor", {}).get("firstBatch", after)
            replies.append({"reply": clean(reply, db), "docs": clean(docs, db)})
        else:
            wire, reply = send(wire, uri, db, step)
            replies.append(clean(reply, db))
    wire, _ = send(wire, uri, db, {"dropDatabase": 1})
    wire.close()
    return replies


def parts(want, got):
    """The halves of a case that differ: the reply, the documents, or both."""
    if not ("docs" in want and "docs" in got):
        return [("", want, got)]
    return [
        (f" {key}", want[key], got[key])
        for key in ("reply", "docs")
        if repr(want[key]) != repr(got[key])
    ]


def show(step):
    if isinstance(step, tuple):
        _, seed, command = step
        return f"{command!r}"[:400]
    return repr(step)[:400]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "900"))

    # Self-check: the reference must seed, update and read back as the probe assumes.
    check = run_scenario(
        mongod_uri,
        0,
        [
            case({"_id": 1, "a": 1}, upd({"_id": 1}, {"$inc": {"a": 1}})),
            case([], upd({"_id": 2}, {"$set": {"a": 1}}, upsert=True)),
            case({"_id": 1, "a": 1}, fam(query={"_id": 1}, remove=True)),
        ],
    )
    want = [
        ({"n": 1, "nModified": 1, "ok": "double(1.0)"}, [{"_id": 1, "a": 2}]),
        (
            {"n": 1, "upserted": [{"index": 0, "_id": 2}], "nModified": 0, "ok": "double(1.0)"},
            [{"_id": 2, "a": 1}],
        ),
        (None, []),
    ]
    for got, (reply, docs) in zip(check, want, strict=True):
        if (reply is not None and got.get("reply") != reply) or got.get("docs") != docs:
            print("self-check failed on mongod:", got, file=sys.stderr)
            return 2
    if check[2]["reply"].get("value") != {"_id": 1, "a": 1}:
        print("self-check failed on mongod:", check[2], file=sys.stderr)
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
        print(f"\n--- {name} ({len(bad)} of {len(steps)})")
        for i in bad:
            print(f"  step {i}: {show(steps[i])}")
            for part, w, g in parts(want[i], got[i]):
                print(f"    mongod{part}: {str(w)[:width]}")
                print(f"    server{part}: {str(g)[:width]}")
    print(
        f"\n=== update and findAndModify: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
