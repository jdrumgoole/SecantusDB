"""Differential-probe `insert`, `delete`, `count` and `distinct` against a real mongod.

What each command accepts as an argument, what it stores or removes, and what
it answers: for every case the whole reply is compared AND the documents the
collection holds afterwards, in the order the collection holds them. A write
that succeeds and leaves different documents behind is the failure this is
here to find, and the reply alone does not show it.

A `case` runs against a collection of its own: the collection is dropped,
the `setup` commands run, the seed documents are inserted, the command runs,
and every document is read back. Commands go over a raw connection, so an
argument a driver would refuse to send still reaches the server.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 \
        python tools/probes/insert_delete_count_distinct.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import datetime
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Binary, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp  # noqa: E402
from bson.errors import InvalidBSON  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
RUN = f"{os.getpid() % 100000}"

DROP = {"$clusterTime", "operationTime", "opTime", "electionId"}

BAD = ["x", 2.5, -1, None, True, {}, [1], Int64(2), 0, Decimal128("2"), 1]
# No small positive budget: whether one expires is a race on mongod itself.
TIMES = ["x", 2.5, -1, None, True, {}, [1], Int64(90000), 0, Decimal128("90000"), 90000]
BOOLS = [True, False, 1, 0, "x", None, {}, 2.5]
DATE = datetime.datetime(2020, 1, 2, 3, 4, 5)
OID = ObjectId("0123456789abcdef01234567")
NAN = float("nan")

S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


def case(seed, command, setup=()):
    """Run `command` against a fresh collection holding `seed`."""
    return ("case", seed if isinstance(seed, list) else [seed], command, list(setup))


def ins(*documents, **top):
    return {"insert": "c", "documents": list(documents), **top}


def dele(q, limit=0, **more):
    """One delete statement; `top` holds fields for the command itself."""
    top = more.pop("top", {})
    return {"delete": "c", "deletes": [{"q": q, "limit": limit, **more}], **top}


def count(**fields):
    return {"count": "c", **fields}


def distinct(key, **fields):
    return {"distinct": "c", "key": key, **fields}


def index(key, **options):
    name = options.pop("name", "_".join(f"{k}_{v}" for k, v in key.items()))
    return {"createIndexes": "c", "indexes": [{"key": key, "name": name, **options}]}


def nested(depth, leaf=1):
    doc = leaf
    for _ in range(depth):
        doc = {"a": doc}
    return doc


FIND = {"find": "c", "batchSize": 500}

# ------------------------------------------------------------------- insert

scenario(
    "insert: the documents argument",
    *[case([], {"insert": "c", "documents": v}) for v in BAD],
    case([], {"insert": "c"}),
    case([], {"insert": "c", "documents": []}),
    case([], {"insert": "c", "documents": [{}]}),
    case([], {"insert": "c", "documents": [{"_id": 1}, 2]}),
    case([], {"insert": "c", "documents": [{"_id": 1}, "x", {"_id": 2}]}),
    case([], {"insert": "c", "documents": [{"_id": 1}, None]}),
    case([], {"insert": "c", "documents": [[{"_id": 1}]]}),
    case([], {"insert": "c", "documents": {"0": {"_id": 1}}}),
    case([], {"insert": "c", "documents": [{"_id": 1}], "ordered": False}),
)
scenario(
    "insert: the collection name",
    *[case([], {"insert": v, "documents": [{"_id": 1}]}) for v in BAD],
    *[
        case([], {"insert": name, "documents": [{"_id": 1}]})
        for name in [
            "",
            "a$b",
            "$a",
            ".a",
            "a.",
            "a..b",
            "system.x",
            "system.views",
            "system.profile",
            "system.buckets.x",
            "system.js",
            "a b",
            "x" * 250,
            "x" * 255,
            "x" * 256,
        ]
    ],
)
scenario(
    "insert: the _id it accepts",
    *[
        case([], ins({"_id": v, "a": 1}))
        for v in [
            [1],
            [],
            Regex("a"),
            None,
            {},
            {"a": 1},
            {"$a": 1},
            {"a": {"$b": 1}},
            {"a.b": 1},
            {"$oid": "0123456789abcdef01234567"},
            {"$numberLong": "1"},
            MinKey(),
            MaxKey(),
            NAN,
            float("inf"),
            -0.0,
            Timestamp(0, 0),
            Timestamp(5, 6),
            True,
            "",
            DATE,
            Int64(7),
            Decimal128("7.0"),
            Decimal128("NaN"),
            Binary(b"ab"),
            Binary(b"ab", 4),
            OID,
        ]
    ],
)
scenario(
    "insert: where _id goes",
    case([], ins({"a": 1})),
    case([], ins({"a": 1, "_id": 5})),
    case([], ins({"a": 1, "_id": 5, "b": 2})),
    case([], ins({"z": 1, "a": 2})),
    case([], ins({"a": {"_id": 1}})),
    case([], ins({"a": 1}, {"_id": 2}, {"b": 1})),
    case([], ins({"_id": 5, "_id2": 1})),
)
scenario(
    "insert: field names",
    *[
        case([], ins({"_id": 1, **doc}))
        for doc in [
            {"$a": 1},
            {"$set": {"a": 1}},
            {"a.b": 1},
            {"a.": 1},
            {".a": 1},
            {"": 1},
            {"a": {"$b": 1}},
            {"a": {"b.c": 1}},
            {"a": {"": 1}},
            {"a": [{"$b": 1}]},
            {"$id": 1},
            {"$ref": "x"},
            {"$db": "x"},
            {"$ref": "x", "$id": 1},
            {"$ref": "x", "$id": 1, "$db": "y"},
            {"a": {"$ref": "x", "$id": 1}},
            {"a": {"$id": 1}},
            {"a": {"$ref": 1, "$id": 1}},
            {"$$a": 1},
            {"a$": 1},
            {"a$b": 1},
            {"$": 1},
            {"$a.b": 1},
            {"é": 1},
        ]
    ],
)
scenario(
    "insert: an empty timestamp",
    case([], ins({"_id": 1, "t": Timestamp(0, 0)})),
    case([], ins({"_id": 1, "a": {"t": Timestamp(0, 0)}})),
    case([], ins({"_id": 1, "a": [Timestamp(0, 0)]})),
    case([], ins({"_id": 1, "t": Timestamp(0, 0), "u": Timestamp(0, 0)})),
    case([], ins({"_id": 1, "t": Timestamp(0, 1)})),
    case([], ins({"_id": 1, "t": Timestamp(1, 0)})),
    case([], ins({"t": Timestamp(0, 0), "_id": 1})),
    case([], ins({"_id": 1, "t": Timestamp(0, 0)}, bypassEmptyTsReplacement=True)),
    case([], ins({"_id": 1, "t": Timestamp(0, 0)}, bypassEmptyTsReplacement=False)),
    case([], ins({"_id": 1, "t": Timestamp(0, 0)}, bypassEmptyTsReplacement="x")),
)
scenario(
    "insert: nesting depth",
    *[
        case([], ins({"_id": 1, "a": nested(d)}))
        for d in [50, 98, 99, 100, 101, 150, 179, 180, 181]
    ],
    *[case([], ins({"_id": 1, "a": nested(d, [[1]])})) for d in [97, 98, 99, 100]],
)
scenario(
    "insert: a duplicate _id",
    *[
        case([{"_id": v}], ins({"_id": v, "a": 1}))
        for v in [
            1,
            "x",
            Int64(2),
            2.5,
            Decimal128("2.50"),
            True,
            DATE,
            OID,
            {"a": 1, "b": "x"},
            Binary(b"ab"),
            Timestamp(5, 6),
            MinKey(),
            'a"b',
            "é",
            -0.0,
            float("inf"),
        ]
    ],
    case([{"_id": 1}], ins({"_id": 1.0})),
    case([{"_id": 1}], ins({"_id": Int64(1)})),
    case([{"_id": 1}], ins({"_id": Decimal128("1.00")})),
    case([{"_id": 0.0}], ins({"_id": -0.0})),
    case([{"_id": {"a": 1, "b": 2}}], ins({"_id": {"b": 2, "a": 1}})),
    case([{"_id": {"a": 1}}], ins({"_id": {"a": 1.0}})),
    case([{"_id": "a"}], ins({"_id": "A"})),
)
scenario(
    "insert: ordered and the errors in a batch",
    case([{"_id": 2}], ins({"_id": 1}, {"_id": 2}, {"_id": 3})),
    case([{"_id": 2}], ins({"_id": 1}, {"_id": 2}, {"_id": 3}, ordered=False)),
    case([{"_id": 2}], ins({"_id": 1}, {"_id": 2}, {"_id": 2}, {"_id": 3}, ordered=False)),
    case([], ins({"_id": 1}, {"_id": 1}, {"_id": 1}, ordered=False)),
    case([], ins({"_id": 1}, {"_id": 1}, {"_id": 1})),
    case([], ins({"_id": 1}, {"_id": [1]}, {"_id": 3})),
    case([], ins({"_id": 1}, {"_id": [1]}, {"_id": 3}, ordered=False)),
    case([], ins({"_id": 1}, {"_id": 2, "a": nested(150)}, {"_id": 3}, ordered=False)),
    case([], ins({"_id": 1}, {"_id": Regex("a")}, {"_id": 1}, {"_id": 4}, ordered=False)),
    *[case([], ins({"_id": 1}, {"_id": 1}, {"_id": 2}, ordered=v)) for v in BOOLS],
)
scenario(
    "insert: a unique index",
    case({"_id": 1, "a": 1}, ins({"_id": 2, "a": 1}), [index({"a": 1}, unique=True)]),
    case({"_id": 1, "a": 1}, ins({"_id": 2, "a": 1.0}), [index({"a": 1}, unique=True)]),
    case({"_id": 1}, ins({"_id": 2}), [index({"a": 1}, unique=True)]),
    case({"_id": 1, "a": None}, ins({"_id": 2}), [index({"a": 1}, unique=True)]),
    case({"_id": 1}, ins({"_id": 2}), [index({"a": 1}, unique=True, sparse=True)]),
    case({"_id": 1, "a": [1, 2]}, ins({"_id": 2, "a": [2, 3]}), [index({"a": 1}, unique=True)]),
    case({"_id": 1, "a": [1, 2]}, ins({"_id": 2, "a": [3, 3]}), [index({"a": 1}, unique=True)]),
    case(
        {"_id": 1, "a": 1, "b": "x"},
        ins({"_id": 2, "a": 1, "b": "x"}),
        [index({"a": 1, "b": -1}, unique=True)],
    ),
    case(
        {"_id": 1, "a": 1, "b": "x"},
        ins({"_id": 2, "a": 1, "b": "y"}),
        [index({"a": 1, "b": -1}, unique=True)],
    ),
    case(
        {"_id": 1, "a": {"b": 1}}, ins({"_id": 2, "a": {"b": 1}}), [index({"a.b": 1}, unique=True)]
    ),
    case(
        {"_id": 1, "a": "x"},
        ins({"_id": 2, "a": "X"}),
        [index({"a": 1}, unique=True, collation={"locale": "en", "strength": 2})],
    ),
    case(
        {"_id": 1, "a": 1, "s": "y"},
        ins({"_id": 2, "a": 1, "s": "n"}, {"_id": 3, "a": 1, "s": "y"}),
        [index({"a": 1}, unique=True, partialFilterExpression={"s": "y"})],
    ),
    case(
        {"_id": 1, "a": 1, "b": 1},
        ins({"_id": 1, "a": 1, "b": 1}),
        [index({"a": 1}, unique=True), index({"b": 1}, unique=True)],
    ),
    case(
        {"_id": 1, "a": 1, "b": 1},
        ins({"_id": 2, "a": 1, "b": 1}),
        [index({"b": 1}, unique=True), index({"a": 1}, unique=True)],
    ),
    case(
        {"_id": 1, "a": DATE},
        ins({"_id": 2, "a": DATE}, {"_id": 3, "a": "x" * 200}, {"_id": 4, "a": "x" * 200}),
        [index({"a": 1}, unique=True)],
    ),
)
scenario(
    "insert: the other arguments",
    *[case([], ins({"_id": 1}, bypassDocumentValidation=v)) for v in BOOLS],
    *[case([], ins({"_id": 1}, maxTimeMS=v)) for v in TIMES],
    *[case([], ins({"_id": 1}, comment=v)) for v in ["x", 1, {"a": 1}, None, [1]]],
    *[
        case([], ins({"_id": 1}, writeConcern=v))
        for v in [
            {},
            {"w": 1},
            {"w": 0},
            {"w": "majority"},
            {"w": 2},
            {"w": "x"},
            {"w": 1, "j": True},
            {"w": 1, "wtimeout": 10},
            {"x": 1},
            "x",
            1,
            None,
            [],
            {"w": -1},
            {"w": 1.5},
            {"w": {"a": 1}},
            {"w": 1, "j": "x"},
            {"w": 1, "fsync": True},
            {"w": 1, "j": True, "fsync": True},
            {"wtimeout": "x"},
        ]
    ],
    case([], ins({"_id": 1}, nope=1)),
    case([], ins({"_id": 1}, let={"a": 1})),
    case([], ins({"_id": 1}, collation={"locale": "en"})),
    case([], ins({"_id": 1}, hint="x")),
    case([], ins({"_id": 1}, upsert=True)),
    case([], ins({"_id": 1}, stmtId=1)),
    case([], ins({"_id": 1}, stmtIds=[1])),
    case([], ins({"_id": 1}, readConcern={"level": "local"})),
    case([], ins({"_id": 1}, isTimeseriesNamespace=True)),
    case([], ins({"_id": 1}, encryptionInformation={"type": 1})),
    case([], ins({"_id": 1}, lsid=1)),
    case([], ins({"_id": 1}, txnNumber=Int64(1))),
    case([], ins({"_id": 1}, autocommit=False)),
    case([], ins({"_id": 1}, apiVersion="1")),
    case([], ins({"_id": 1}, apiVersion="2")),
    case([], ins({"_id": 1}, apiVersion="1", apiStrict=True)),
)
scenario(
    "insert: where it may not write",
    case([], ins({"_id": 1}), [{"create": "v", "viewOn": "c", "pipeline": []}]),
    ("case", [], {"insert": "v", "documents": [{"_id": 1}]}, [{"create": "v", "viewOn": "c"}]),
    case(
        [],
        ins({"_id": 1, "a": "x"}, {"_id": 2, "a": 1}, {"_id": 3, "a": "y"}, ordered=False),
        [{"create": "c", "validator": {"a": {"$type": "int"}}}],
    ),
    case(
        [],
        ins({"_id": 1, "a": "x"}, bypassDocumentValidation=True),
        [{"create": "c", "validator": {"a": {"$type": "int"}}}],
    ),
    case(
        [],
        ins(*[{"_id": i} for i in range(6)]),
        [{"create": "c", "capped": True, "size": 4096, "max": 3}],
    ),
    case([], {"insert": "c", "documents": [{"_id": i} for i in range(3)], "$db": "ignored"}),
)
scenario(
    "insert: values it stores",
    case(
        [],
        ins(
            {
                "_id": 1,
                "i": 1,
                "l": Int64(1),
                "d": 1.0,
                "m": Decimal128("1.00"),
                "nz": -0.0,
                "nan": NAN,
                "o": {"$numberLong": "1"},
                "oid": {"$oid": "0123456789abcdef01234567"},
                "dt": {"$date": "2020-01-01T00:00:00Z"},
                "re": Regex("a", "ix"),
                "mk": MinKey(),
                "b": Binary(b"\x00\x01", 5),
                "arr": [[], {}, [[1]], None],
                "s": "a\x00b",
                "ts": Timestamp(1, 2),
            }
        ),
    ),
    case([], ins({"_id": 1, "a": 1, "a2": 2, "b": {"z": 1, "a": 2}})),
)

# ------------------------------------------------------------------- delete

ROWS = [
    {"_id": 1, "a": 3, "s": "x", "t": [1, 2]},
    {"_id": 2, "a": 1, "s": "X", "t": [2, 3]},
    {"_id": 3, "a": 2, "s": "y"},
    {"_id": 4, "a": 1, "s": "x", "t": []},
    {"_id": 5, "s": "z", "a": None},
]

scenario(
    "delete: the deletes argument",
    *[case(ROWS, {"delete": "c", "deletes": v}) for v in BAD],
    case(ROWS, {"delete": "c"}),
    case(ROWS, {"delete": "c", "deletes": []}),
    case(ROWS, {"delete": "c", "deletes": [{}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {}}]}),
    case(ROWS, {"delete": "c", "deletes": [{"limit": 0}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0}, 2]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0}, {"q": {}}]}),
    case(ROWS, {"delete": "c", "deletes": {"q": {}, "limit": 0}}),
    case(ROWS, {"delete": "c", "deletes": [{"limit": 1, "q": {"a": 1}}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "nope": 1}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "multi": True}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "upsert": True}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "u": {}}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "sort": {"a": 1}}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "arrayFilters": []}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "comment": "x"}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "let": {"a": 1}}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "c": {"a": 1}}]}),
    case(ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0, "sampleId": 1}]}),
)
scenario(
    "delete: limit",
    *[
        case(ROWS, dele({"a": 1}, v))
        for v in [
            0,
            1,
            2,
            -1,
            1.0,
            0.0,
            1.5,
            0.5,
            "1",
            True,
            False,
            Int64(1),
            Int64(0),
            Int64(2),
            Decimal128("1"),
            Decimal128("0"),
            Decimal128("1.5"),
            None,
            NAN,
            float("inf"),
            {},
            [1],
            2**31,
            Int64(2**40),
        ]
    ],
)
scenario(
    "delete: the query",
    *[case(ROWS, dele(v)) for v in ["x", 1, None, [], [{"a": 1}], True, 2.5]],
    *[
        case(ROWS, dele(q))
        for q in [
            {},
            {"_id": 1},
            {"_id": {"$in": [1, 3, 9]}},
            {"a": None},
            {"a": {"$exists": False}},
            {"t": 2},
            {"t": []},
            {"t": {"$size": 0}},
            {"a": {"$gt": 1}},
            {"$or": [{"a": 3}, {"s": "y"}]},
            {"$expr": {"$eq": ["$a", 1]}},
            {"$expr": {"$eq": ["$a", "$$v"]}},
            {"a": {"$nope": 1}},
            {"$nope": 1},
            {"a": {"$in": 1}},
            {"a": {"$regex": "("}},
            {"$where": "true"},
            {"$text": {"$search": "x"}},
            {"$comment": "x"},
            {"a": {"$type": "nope"}},
            {"$expr": {"$nope": 1}},
            {"$expr": {"$divide": [1, 0]}},
            {"$jsonSchema": {"required": ["t"]}},
            {"a": {"$mod": [2, 1]}},
            {"s": Regex("^x", "i")},
            {"$and": []},
            {"a": {"$near": [0, 0]}},
        ]
    ],
)
scenario(
    "delete: which document limit 1 removes",
    case(ROWS, dele({"a": 1}, 1)),
    case(ROWS, dele({}, 1)),
    case(ROWS, dele({"a": {"$gte": 1}}, 1)),
    case(ROWS, dele({"a": {"$gte": 1}}, 1), [index({"a": 1})]),
    case(ROWS, dele({"a": {"$gte": 1}}, 1), [index({"a": -1})]),
    case(ROWS, dele({"a": {"$gte": 1}}, 1, hint={"a": 1}), [index({"a": 1})]),
    case(ROWS, dele({"a": {"$gte": 1}}, 1, hint="a_-1"), [index({"a": -1})]),
    case(ROWS, dele({"a": {"$gte": 1}}, 1, hint={"$natural": -1}), [index({"a": 1})]),
    case(ROWS, dele({"a": {"$gte": 1}}, 1, hint={"$natural": 1}), [index({"a": 1})]),
    case(ROWS, dele({}, 1, hint={"$natural": -1})),
    case(ROWS, dele({}, 1, hint={"a": -1}), [index({"a": -1})]),
    case(ROWS, dele({"s": {"$in": ["x", "y"]}}, 1), [index({"s": -1})]),
    case(ROWS, dele({"t": {"$gte": 2}}, 1), [index({"t": 1})]),
    case(ROWS, dele({"_id": {"$gte": 2}}, 1)),
    case(ROWS, dele({"_id": {"$lte": 4}}, 1, hint={"_id": 1})),
)
scenario(
    "delete: hint",
    *[
        case(ROWS, dele({"a": 1}, 0, hint=v), [index({"a": 1})])
        for v in [
            "a_1",
            {"a": 1},
            "nope",
            {"nope": 1},
            {"a": -1},
            "_id_",
            {"_id": 1},
            {"$natural": 1},
            {"$natural": -1},
            {"$natural": 2},
            {"$natural": "x"},
            {"$natural": 1, "a": 1},
            "$natural",
            "",
            {},
            1,
            None,
            [],
            True,
            2.5,
            [["a", 1]],
        ]
    ],
    case([], dele({"a": 1}, 0, hint="nope")),
    case(ROWS, dele({"a": 1}, 0, hint={"a": 1})),
)
scenario(
    "delete: collation",
    *[
        case(ROWS, dele({"s": "x"}, 0, collation=v))
        for v in [
            {"locale": "en", "strength": 2},
            {"locale": "en", "strength": 1},
            {"locale": "en"},
            {"locale": "simple"},
            {"locale": "nope"},
            {},
            {"strength": 2},
            {"locale": "en", "strength": 9},
            {"locale": "en", "nope": 1},
            "en",
            1,
            None,
            [],
            {"locale": 1},
            {"locale": "en", "strength": "x"},
            {"locale": "en", "caseLevel": 1},
            {"locale": "en", "numericOrdering": True},
        ]
    ],
    case(
        ROWS,
        dele({"s": "x"}, 0),
        [{"create": "c", "collation": {"locale": "en", "strength": 2}}],
    ),
    case(
        ROWS,
        dele({"s": "x"}, 0, collation={"locale": "simple"}),
        [{"create": "c", "collation": {"locale": "en", "strength": 2}}],
    ),
    case(
        ROWS,
        dele({"_id": 1}, 0, collation={"locale": "fr"}),
        [{"create": "c", "collation": {"locale": "en", "strength": 2}}],
    ),
)
scenario(
    "delete: several statements",
    case(
        ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 1}, {"q": {"a": 1}, "limit": 1}]}
    ),
    case(
        ROWS, {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 0}, {"q": {"a": 1}, "limit": 0}]}
    ),
    case(
        ROWS,
        {
            "delete": "c",
            "deletes": [
                {"q": {"_id": 1}, "limit": 0},
                {"q": {"a": {"$nope": 1}}, "limit": 0},
                {"q": {"_id": 2}, "limit": 0},
            ],
        },
    ),
    case(
        ROWS,
        {
            "delete": "c",
            "ordered": False,
            "deletes": [
                {"q": {"_id": 1}, "limit": 0},
                {"q": {"a": {"$nope": 1}}, "limit": 0},
                {"q": {"_id": 2}, "limit": 0},
                {"q": {"_id": 3}, "limit": 0, "hint": "nope"},
                {"q": {"_id": 4}, "limit": 7},
                {"q": {"_id": 5}, "limit": 0},
            ],
        },
    ),
    case(
        ROWS,
        {
            "delete": "c",
            "deletes": [
                {"q": {"_id": 1}, "limit": 0},
                {"q": {"_id": 4}, "limit": 7},
                {"q": {"_id": 5}, "limit": 0},
            ],
        },
    ),
    case(
        ROWS,
        {
            "delete": "c",
            "ordered": False,
            "deletes": [
                {"q": {"_id": 1}, "limit": 0},
                {"q": {"_id": 5}, "limit": 0, "nope": 1},
            ],
        },
    ),
    *[case(ROWS, dele({"a": 1}, 0, top={"ordered": v})) for v in BOOLS],
)
scenario(
    "delete: the other arguments",
    *[case(ROWS, {"delete": v, "deletes": [{"q": {}, "limit": 0}]}) for v in BAD],
    *[
        case(ROWS, {"delete": name, "deletes": [{"q": {}, "limit": 0}]})
        for name in ["", "a$b", "$a", ".a", "a.", "system.x", "system.views", "x" * 256, "nope"]
    ],
    *[
        case(ROWS, dele({"$expr": {"$eq": ["$a", "$$v"]}}, 0, top={"let": v}))
        for v in [
            {"v": 1},
            {},
            1,
            "x",
            None,
            [],
            {"v": "$a"},
            {"V": 1},
            {"v": {"$add": [1, 0]}},
            {"$v": 1},
            {"v": "$$NOW"},
        ]
    ],
    *[case(ROWS, dele({"a": 1}, 0, top={"maxTimeMS": v})) for v in TIMES],
    *[case(ROWS, dele({"a": 1}, 0, top={"comment": v})) for v in ["x", {"a": 1}, None]],
    *[
        case(ROWS, dele({"a": 1}, 0, top={"writeConcern": v}))
        for v in [{"w": 1}, {"w": 0}, {"x": 1}, "x", {"w": "nope"}]
    ],
    case(ROWS, dele({"a": 1}, 0, top={"nope": 1})),
    case(ROWS, dele({"a": 1}, 0, top={"bypassDocumentValidation": True})),
    case(ROWS, dele({"a": 1}, 0, top={"collation": {"locale": "en"}})),
    case(ROWS, dele({"a": 1}, 0, top={"hint": "x"})),
    case(ROWS, dele({"a": 1}, 0, top={"limit": 1})),
    case(ROWS, dele({"a": 1}, 0, top={"isTimeseriesNamespace": True})),
    case(ROWS, dele({"a": 1}, 0, top={"encryptionInformation": {"type": 1}})),
    case(ROWS, dele({"a": 1}, 0, top={"readConcern": {"level": "local"}})),
    case(ROWS, dele({"a": 1}, 0, top={"bypassEmptyTsReplacement": True})),
    case(ROWS, dele({"a": 1}, 0, top={"stmtId": 1})),
)
scenario(
    "delete: where it may not write",
    case([], dele({}, 0)),
    case(ROWS, dele({}, 0), [{"create": "v", "viewOn": "c", "pipeline": []}]),
    (
        "case",
        [],
        {"delete": "v", "deletes": [{"q": {}, "limit": 0}]},
        [{"create": "v", "viewOn": "c"}],
    ),
    case(
        [{"_id": i} for i in range(3)],
        dele({"_id": 1}, 0),
        [{"create": "c", "capped": True, "size": 4096}],
    ),
    case(
        [{"_id": i} for i in range(3)],
        dele({}, 1),
        [{"create": "c", "capped": True, "size": 4096}],
    ),
)

# -------------------------------------------------------------------- count

scenario(
    "count: the query",
    *[case(ROWS, count(query=v)) for v in ["x", 1, None, [], True, 2.5, [{"a": 1}]]],
    *[
        case(ROWS, count(query=q))
        for q in [
            {},
            {"a": 1},
            {"a": None},
            {"t": 2},
            {"a": {"$nope": 1}},
            {"$nope": 1},
            {"$expr": {"$eq": ["$a", 1]}},
            {"$expr": {"$nope": 1}},
            {"$where": "true"},
            {"$text": {"$search": "x"}},
            {"a": {"$near": [0, 0]}},
            {"$comment": "x"},
            {"_id": {"$gt": 2}},
        ]
    ],
    case(ROWS, count()),
    case([], count()),
    case([], count(query={"a": 1})),
    case([], count(query={"a": {"$nope": 1}})),
)
NUMBERS = [
    0,
    1,
    2,
    -1,
    -2,
    1.0,
    1.5,
    -1.5,
    0.5,
    "1",
    True,
    None,
    Int64(2),
    Int64(-2),
    Decimal128("2"),
    Decimal128("1.5"),
    NAN,
    float("inf"),
    float("-inf"),
    {},
    [1],
    2**31,
    Int64(2**40),
    Int64(-(2**63)),
    100,
]
scenario("count: limit", *[case(ROWS, count(limit=v)) for v in NUMBERS])
scenario(
    "count: limit with a query",
    *[case(ROWS, count(query={"a": {"$gte": 1}}, limit=v)) for v in NUMBERS],
)
scenario("count: skip", *[case(ROWS, count(skip=v)) for v in NUMBERS])
scenario(
    "count: skip with a query",
    *[case(ROWS, count(query={"a": {"$gte": 1}}, skip=v)) for v in NUMBERS],
)
scenario(
    "count: skip and limit together",
    case(ROWS, count(skip=1, limit=2)),
    case(ROWS, count(skip=4, limit=2)),
    case(ROWS, count(skip=1, limit=-2)),
    case(ROWS, count(skip=9, limit=2)),
    case(ROWS, count(query={"a": 1}, skip=1, limit=5)),
    case(ROWS, count(query={"a": 1}, skip=1, limit=0)),
    case([], count(skip=1, limit=2)),
    case([], count(skip=-1)),
    case([], count(limit="x")),
)
scenario(
    "count: hint",
    *[
        case(ROWS, count(query=q, hint=v), [index({"a": 1}), index({"s": 1}, sparse=True)])
        for q in [{}, {"a": 1}]
        for v in [
            "a_1",
            {"a": 1},
            "nope",
            {"nope": 1},
            "_id_",
            {"$natural": 1},
            {"$natural": -1},
            {"$natural": 2},
            "$natural",
            "",
            {},
            1,
            None,
            [],
            True,
            "s_1",
        ]
    ],
    case([], count(hint="nope")),
    case([], count(query={"a": 1}, hint="nope")),
    case(
        [{"_id": 1, "a": 1}, {"_id": 2}],
        count(query={"a": None}, hint="a_1"),
        [index({"a": 1}, sparse=True)],
    ),
    case(
        [{"_id": 1, "a": 1}, {"_id": 2}],
        count(hint="a_1"),
        [index({"a": 1}, sparse=True)],
    ),
    case(
        [{"_id": 1, "a": 1, "s": "y"}, {"_id": 2, "a": 1}],
        count(query={"a": 1}, hint="a_1"),
        [index({"a": 1}, partialFilterExpression={"s": "y"})],
    ),
)
scenario(
    "count: collation",
    *[
        case(ROWS, count(query={"s": "x"}, collation=v))
        for v in [
            {"locale": "en", "strength": 2},
            {"locale": "en"},
            {"locale": "simple"},
            {"locale": "nope"},
            {},
            {"strength": 2},
            {"locale": "en", "strength": 9},
            {"locale": "en", "nope": 1},
            "en",
            1,
            None,
            [],
        ]
    ],
    case(ROWS, count(collation={"locale": "nope"})),
    case([], count(collation={"locale": "nope"})),
    case(
        ROWS,
        count(query={"s": "x"}),
        [{"create": "c", "collation": {"locale": "en", "strength": 2}}],
    ),
)
scenario(
    "count: the other arguments",
    *[case(ROWS, {"count": v}) for v in BAD],
    *[
        case(ROWS, {"count": name})
        for name in ["", "a$b", "$a", ".a", "system.x", "x" * 256, "nope"]
    ],
    *[case(ROWS, count(maxTimeMS=v)) for v in TIMES],
    *[case(ROWS, count(comment=v)) for v in ["x", {"a": 1}, None, 1]],
    *[
        case(ROWS, count(readConcern=v))
        for v in [
            {},
            {"level": "local"},
            {"level": "majority"},
            {"level": "available"},
            {"level": "linearizable"},
            {"level": "snapshot"},
            {"level": "nope"},
            {"level": 1},
            {"nope": 1},
            "local",
            1,
            None,
            {"afterClusterTime": Timestamp(1, 1)},
            {"level": "local", "afterClusterTime": Timestamp(1, 1)},
            {"level": "snapshot", "atClusterTime": Timestamp(1, 1)},
        ]
    ],
    case(ROWS, count(nope=1)),
    case(ROWS, count(fields={"a": 1})),
    case(ROWS, count(sort={"a": 1})),
    case(ROWS, count(let={"a": 1})),
    case(ROWS, count(query={"$expr": {"$eq": ["$a", "$$v"]}}, let={"v": 1})),
    case(ROWS, count(batchSize=1)),
    case(ROWS, count(writeConcern={"w": 1})),
    case(ROWS, count(encryptionInformation={"type": 1})),
    case(ROWS, count(mirrored=True)),
    case(ROWS, count(sampleId=1)),
    case(ROWS, count(rawData=True)),
    case(ROWS, count(includeQueryStatsMetrics=True)),
    case(ROWS, count(**{"$queryOptions": {}})),
    case(ROWS, count(**{"$readPreference": {"mode": "secondary"}})),
    case(ROWS, count(), [{"create": "v", "viewOn": "c", "pipeline": [{"$match": {"a": 1}}]}]),
    (
        "case",
        ROWS,
        {"count": "v"},
        [{"create": "v", "viewOn": "c", "pipeline": [{"$match": {"a": 1}}]}],
    ),
    (
        "case",
        ROWS,
        {"count": "v", "query": {"_id": 2}, "skip": 0, "limit": 5},
        [{"create": "v", "viewOn": "c", "pipeline": [{"$match": {"a": 1}}]}],
    ),
    (
        "case",
        ROWS,
        {"count": "v", "hint": "nope"},
        [{"create": "v", "viewOn": "c", "pipeline": []}],
    ),
)

# ----------------------------------------------------------------- distinct

VALUES = [
    {"_id": 1, "a": 2, "o": {"b": 1}, "t": [1, 2, [3, 4]], "n": 1, "s": "b"},
    {"_id": 2, "a": 1, "o": {"b": [1, 5]}, "t": [2, 2, {"x": 1}], "n": 1.0, "s": "B"},
    {
        "_id": 3,
        "a": 2,
        "o": [{"b": 7}, {"b": [8, [9]]}, {"c": 1}, 3],
        "t": [],
        "n": Int64(1),
        "s": "a",
    },
    {"_id": 4, "a": None, "o": {"b": None}, "t": None, "n": Decimal128("1.0"), "s": "b"},
    {"_id": 5, "o": {}, "n": -0.0, "s": "é"},
    {"_id": 6, "a": "x", "o": 5, "t": [[]], "n": 0, "s": "A"},
    {"_id": 7, "a": {"z": 1}, "o": {"b": {"c": 1}}, "t": [{"x": 1}, {"x": [2, 3]}], "n": NAN},
    {"_id": 8, "a": [3, 1], "o": {"b": 1}, "t": [None], "n": NAN, "s": None},
    {"_id": 9, "a": True, "n": Decimal128("NaN"), "t": {"x": 4}},
    {"_id": 10, "a": 1.0, "n": 2, "t": [{"x": 4}, 5]},
]

scenario(
    "distinct: the key",
    *[case(VALUES, {"distinct": "c", "key": v}) for v in BAD],
    case(VALUES, {"distinct": "c"}),
    *[
        case(VALUES, distinct(key))
        for key in [
            "a",
            "_id",
            "o",
            "o.b",
            "o.b.c",
            "o.c",
            "t",
            "t.x",
            "t.0",
            "t.1",
            "t.2.x",
            "t.2.0",
            "o.0",
            "o.1.b",
            "o.1.b.1",
            "n",
            "s",
            "nope",
            "nope.x",
            "",
            "a.",
            ".a",
            "a..b",
            "$a",
            "a.$b",
            "$",
            "a.z",
            "_id.x",
            "t.x.0",
        ]
    ],
    case([], distinct("a")),
    case([], distinct("")),
    case([], {"distinct": "c", "key": 1}),
)
scenario(
    "distinct: with an index",
    *[
        case(
            VALUES,
            distinct(key, **more),
            [index({"a": 1}), index({"n": -1}), index({"o.b": 1}), index({"s": 1, "a": 1})],
        )
        for key, more in [
            ("a", {}),
            ("n", {}),
            ("o.b", {}),
            ("s", {}),
            ("a", {"query": {"a": {"$gte": 1}}}),
            ("a", {"query": {"s": "b"}}),
            ("s", {"query": {"a": 2}}),
            ("a", {"hint": "a_1"}),
            ("a", {"hint": {"$natural": -1}}),
            ("a", {"hint": {"$natural": 1}}),
            ("n", {"hint": "a_1"}),
            ("n", {"hint": "n_-1"}),
            ("s", {"hint": "s_1_a_1"}),
            ("_id", {"hint": "a_1"}),
            ("a", {"query": {"_id": {"$gt": 5}}}),
        ]
    ],
)
scenario(
    "distinct: the query",
    *[case(VALUES, distinct("a", query=v)) for v in ["x", 1, None, [], True, 2.5]],
    *[
        case(VALUES, distinct("a", query=q))
        for q in [
            {},
            {"a": 2},
            {"_id": {"$lt": 4}},
            {"a": {"$nope": 1}},
            {"$nope": 1},
            {"$expr": {"$eq": ["$a", 2]}},
            {"$expr": {"$nope": 1}},
            {"$where": "true"},
            {"$text": {"$search": "x"}},
            {"nope": 1},
        ]
    ],
    case([], distinct("a", query={"a": {"$nope": 1}})),
)
scenario(
    "distinct: hint",
    *[
        case(VALUES, distinct("a", hint=v), [index({"a": 1}), index({"s": 1}, sparse=True)])
        for v in [
            {"a": 1},
            "nope",
            {"nope": 1},
            "_id_",
            {"$natural": 2},
            "$natural",
            "",
            {},
            1,
            None,
            [],
            True,
            "s_1",
        ]
    ],
    case(VALUES, distinct("s", hint="s_1"), [index({"s": 1}, sparse=True)]),
    case([], distinct("a", hint="nope")),
)
scenario(
    "distinct: collation",
    *[
        case(VALUES, distinct("s", collation=v))
        for v in [
            {"locale": "en", "strength": 2},
            {"locale": "en", "strength": 1},
            {"locale": "en"},
            {"locale": "simple"},
            {"locale": "nope"},
            {},
            {"strength": 2},
            {"locale": "en", "nope": 1},
            "en",
            1,
            None,
            [],
        ]
    ],
    case(VALUES, distinct("s", query={"s": "b"}, collation={"locale": "en", "strength": 2})),
    case([], distinct("s", collation={"locale": "nope"})),
    case(
        VALUES,
        distinct("s"),
        [{"create": "c", "collation": {"locale": "en", "strength": 2}}],
    ),
    case(
        VALUES,
        distinct("s"),
        [index({"s": 1}, name="s_en", collation={"locale": "en", "strength": 2})],
    ),
    case(
        VALUES,
        distinct("s", collation={"locale": "en", "strength": 2}),
        [index({"s": 1}, name="s_en", collation={"locale": "en", "strength": 2})],
    ),
)
scenario(
    "distinct: the other arguments",
    *[case(VALUES, {"distinct": v, "key": "a"}) for v in BAD],
    *[
        case(VALUES, {"distinct": name, "key": "a"})
        for name in ["", "a$b", "$a", ".a", "system.x", "x" * 256, "nope"]
    ],
    *[case(VALUES, distinct("a", maxTimeMS=v)) for v in TIMES],
    *[case(VALUES, distinct("a", comment=v)) for v in ["x", {"a": 1}, None, 1]],
    *[
        case(VALUES, distinct("a", readConcern=v))
        for v in [
            {},
            {"level": "local"},
            {"level": "majority"},
            {"level": "snapshot"},
            {"level": "nope"},
            "local",
            None,
        ]
    ],
    case(VALUES, distinct("a", nope=1)),
    case(VALUES, distinct("a", limit=1)),
    case(VALUES, distinct("a", skip=1)),
    case(VALUES, distinct("a", sort={"a": 1})),
    case(VALUES, distinct("a", let={"a": 1})),
    case(VALUES, distinct("a", batchSize=1)),
    case(VALUES, distinct("a", projection={"a": 1})),
    case(VALUES, distinct("a", writeConcern={"w": 1})),
    case(VALUES, distinct("a", mirrored=True)),
    case(VALUES, distinct("a", sampleId=1)),
    case(VALUES, distinct("a", rawData=True)),
    case(VALUES, distinct("a", includeQueryStatsMetrics=True)),
    (
        "case",
        VALUES,
        {"distinct": "v", "key": "a"},
        [{"create": "v", "viewOn": "c", "pipeline": [{"$match": {"_id": {"$lt": 5}}}]}],
    ),
    (
        "case",
        VALUES,
        {"distinct": "v", "key": "t", "query": {"_id": {"$gt": 1}}},
        [{"create": "v", "viewOn": "c", "pipeline": [{"$match": {"_id": {"$lt": 5}}}]}],
    ),
    (
        "case",
        VALUES,
        {"distinct": "v", "key": "o.b"},
        [{"create": "v", "viewOn": "c", "pipeline": []}],
    ),
    (
        "case",
        VALUES,
        {"distinct": "v", "key": "a", "hint": "nope"},
        [{"create": "v", "viewOn": "c", "pipeline": []}],
    ),
)
scenario(
    "distinct: which value stands for equal ones",
    *[
        case(docs, distinct("a"))
        for docs in [
            [{"_id": 1, "a": 1}, {"_id": 2, "a": 1.0}, {"_id": 3, "a": Int64(1)}],
            [{"_id": 1, "a": 1.0}, {"_id": 2, "a": 1}],
            [{"_id": 1, "a": Decimal128("1.00")}, {"_id": 2, "a": 1}],
            [{"_id": 1, "a": 0.0}, {"_id": 2, "a": -0.0}],
            [{"_id": 1, "a": -0.0}, {"_id": 2, "a": 0}],
            [{"_id": 1, "a": NAN}, {"_id": 2, "a": Decimal128("NaN")}, {"_id": 3, "a": NAN}],
            [
                {"_id": 1, "a": {"x": 1, "y": 2}},
                {"_id": 2, "a": {"y": 2, "x": 1}},
                {"_id": 3, "a": {"x": 1.0, "y": 2}},
            ],
            [{"_id": 1, "a": [1, 2]}, {"_id": 2, "a": [2, 1]}],
            [{"_id": 1, "a": [[1, 2]]}, {"_id": 2, "a": [[1, 2], [1, 2]]}],
            [{"_id": 1, "a": None}, {"_id": 2}, {"_id": 3, "a": []}],
            [
                {"_id": 1, "a": "b"},
                {"_id": 2, "a": 2},
                {"_id": 3, "a": "a"},
                {"_id": 4, "a": 1},
                {"_id": 5, "a": True},
                {"_id": 6, "a": None},
                {"_id": 7, "a": DATE},
                {"_id": 8, "a": {"x": 1}},
            ],
            [{"_id": 1, "a": 3}, {"_id": 2, "a": 1}, {"_id": 3, "a": 2}, {"_id": 4, "a": 1}],
            [{"_id": 1, "a": [3, 1]}, {"_id": 2, "a": [2, 1]}],
            [{"_id": 1, "a": Regex("a")}, {"_id": 2, "a": Regex("a", "i")}, {"_id": 3, "a": "a"}],
            [
                {"_id": 1, "a": MinKey()},
                {"_id": 2, "a": MaxKey()},
                {"_id": 3, "a": Timestamp(1, 1)},
                {"_id": 4, "a": Binary(b"a")},
                {"_id": 5, "a": OID},
            ],
        ]
    ],
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
        return value if value == OID else "OID"
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
    except InvalidBSON as exc:
        return wire, {"probe": f"REPLY NOT DECODED {exc}"}
    except (ConnectionError, OSError) as exc:
        wire.close()
        return Wire(uri), {"probe": f"CONNECTION {type(exc).__name__}"}


def run_scenario(uri, number, steps):
    db = f"idcd{RUN}_{number}"
    wire = Wire(uri)
    replies = []
    for _, seed, command, setup in steps:
        wire, _ = send(wire, uri, db, {"drop": "c"})
        wire, _ = send(wire, uri, db, {"drop": "v"})
        failed = None
        for prepare in setup:
            wire, made = send(wire, uri, db, prepare)
            if made.get("ok") != 1.0:
                failed = made
        if seed and failed is None:
            wire, made = send(wire, uri, db, {"insert": "c", "documents": seed})
            if made.get("n") != len(seed):
                failed = made
        if failed is not None:
            replies.append({"probe": "SETUP", "reply": clean(failed, db)})
            continue
        wire, reply = send(wire, uri, db, command)
        wire, after = send(wire, uri, db, FIND)
        docs = after.get("cursor", {}).get("firstBatch", after)
        if command.get("distinct") == "v" and isinstance(reply.get("values"), list):
            # Over a view mongod returns the values in no fixed order.
            reply["values"] = sorted(reply["values"], key=repr)
        replies.append({"reply": clean(reply, db), "docs": clean(docs, db)})
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
    _, _, command, setup = step
    text = f"{command!r}"[:400]
    return f"{text}  after {setup!r}"[:700] if setup else text


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "900"))

    # Self-check: the reference must set up, write, count and read back as the probe assumes.
    check = run_scenario(
        mongod_uri,
        0,
        [
            case([], ins({"_id": 1, "a": 1})),
            case(ROWS, dele({"a": 1}, 1), [index({"a": 1})]),
            case(ROWS, count(query={"a": 1})),
            case(ROWS, distinct("s", query={"a": 1})),
        ],
    )
    want = [
        ({"n": 1, "ok": "double(1.0)"}, 1),
        ({"n": 1, "ok": "double(1.0)"}, 4),
        ({"n": 2, "ok": "double(1.0)"}, 5),
        ({"values": ["X", "x"], "ok": "double(1.0)"}, 5),
    ]
    for got, (reply, held) in zip(check, want, strict=True):
        if got.get("reply") != reply or len(got.get("docs", [])) != held:
            print("self-check failed on mongod:", got, file=sys.stderr)
            return 2

    divergent = replies = replies_bad = total = 0
    for number, (name, steps) in enumerate(S, start=1):
        if only and only not in name:
            continue
        total += 1
        want = run_scenario(mongod_uri, number, steps)
        got = run_scenario(server_uri, number, steps)
        replies += len(steps)
        setups = [w for w in want if w.get("probe") == "SETUP"]
        if setups:
            print(f"\n--- {name}: {len(setups)} cases could not be set up on mongod")
            for w in setups:
                print(f"    {str(w)[:width]}")
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
        f"\n=== insert, delete, count and distinct: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
