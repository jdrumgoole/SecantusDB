"""The shapes left open for the Rust MongoDB server, against mongod.

Written for phase 0 of `tasks/rust-mongod-remaining-plan.md` (2026-10-06):
backlog entries with no probe of their own -- `$jsonSchema` (parse and the
failure `errInfo`), write concern, `$project: {_id: 1}`, the positional update,
and the section 7.00 expression entries (negative `$slice`, decimal operands,
`$bucketAuto` over decimals). It compares whole command replies.

**Point it at a REPLICA-SET mongod.** The Rust server presents itself as a
single-node replica set, so a standalone mongod answers write concern
differently (`w: 2` is a pre-flight BadValue there) and the comparison is
wrong rather than informative.

    mongod --replSet rs0 --port 27045 --dbpath <dir>   # then replSetInitiate
    python tools/probes/remaining_shapes.py \\
        "mongodb://127.0.0.1:27045/?directConnection=true" \\
        "mongodb://127.0.0.1:27055/?directConnection=true"

Measured on 8.2.11, 2026-10-06, after phase 1: 3 of 72 divergent, all
known -- the authorised last digit of decimal `$sin` and of a decimal `$log`
over a double base, and a multi-key `w` tag set, which mongod echoes (and names
in its error) in hash-map order. Before phase 1 it was 25 of 58.
"""

import json
import re
import sys

import pymongo
from bson import Decimal128, json_util
from bson.codec_options import CodecOptions, DatetimeConversion

CO = CodecOptions(datetime_conversion=DatetimeConversion.DATETIME_AUTO)
D = Decimal128


def norm(x):
    return json.loads(json_util.dumps(x, json_options=json_util.CANONICAL_JSON_OPTIONS))


def cases():
    S = {
        "bsonType": "object",
        "required": ["a"],
        "properties": {"a": {"bsonType": "int", "minimum": 5}},
    }
    yield (
        "jsonSchema type integer",
        [("create", {"create": "js1", "validator": {"$jsonSchema": {"type": "integer"}}})],
    )
    yield (
        "jsonSchema properties type integer",
        [
            (
                "create",
                {
                    "create": "js1b",
                    "validator": {"$jsonSchema": {"properties": {"a": {"type": "integer"}}}},
                },
            )
        ],
    )
    yield (
        "jsonSchema find type integer",
        [
            (
                "find",
                {
                    "find": "c",
                    "filter": {"$jsonSchema": {"properties": {"a": {"type": "integer"}}}},
                },
            )
        ],
    )
    for name, doc in [
        ("missing required", {"b": 1}),
        ("wrong type", {"a": "x"}),
        ("below minimum", {"a": 2}),
    ]:
        yield (
            f"jsonSchema validation {name}",
            [
                ("create", {"create": "js2", "validator": {"$jsonSchema": S}}),
                ("insert", {"insert": "js2", "documents": [dict(doc, _id=1)]}),
            ],
        )
    yield (
        "jsonSchema enum+pattern+array",
        [
            (
                "create",
                {
                    "create": "js3",
                    "validator": {
                        "$jsonSchema": {
                            "properties": {
                                "e": {"enum": [1, 2]},
                                "p": {"bsonType": "string", "pattern": "^a"},
                                "l": {
                                    "bsonType": "array",
                                    "maxItems": 1,
                                    "items": {"bsonType": "int"},
                                },
                                "o": {
                                    "bsonType": "object",
                                    "additionalProperties": False,
                                    "properties": {"x": {}},
                                },
                            }
                        }
                    },
                },
            ),
            (
                "insert",
                {
                    "insert": "js3",
                    "documents": [
                        {"_id": 1, "e": 3, "p": "b", "l": [1, "x"], "o": {"x": 1, "y": 2}}
                    ],
                },
            ),
        ],
    )
    yield (
        "jsonSchema update failure",
        [
            ("create", {"create": "js4", "validator": {"$jsonSchema": S}}),
            ("insert", {"insert": "js4", "documents": [{"_id": 1, "a": 9}]}),
            ("update", {"update": "js4", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1}}}]}),
        ],
    )
    yield (
        "write concern unknown tag",
        [
            (
                "insert",
                {"insert": "wc", "documents": [{"_id": 1}], "writeConcern": {"w": "noSuchTag"}},
            )
        ],
    )
    yield (
        "write concern w:2 wtimeout",
        [
            (
                "insert",
                {
                    "insert": "wc2",
                    "documents": [{"_id": 1}],
                    "writeConcern": {"w": 2, "wtimeout": 50},
                },
            )
        ],
    )
    yield (
        "write concern w:majority",
        [
            (
                "insert",
                {"insert": "wc3", "documents": [{"_id": 1}], "writeConcern": {"w": "majority"}},
            )
        ],
    )
    yield (
        "write concern bad type",
        [("insert", {"insert": "wc4", "documents": [{"_id": 1}], "writeConcern": {"w": []}})],
    )
    yield (
        "write concern update w:2",
        [
            (
                "update",
                {
                    "update": "wc5",
                    "updates": [{"q": {"_id": 7}, "u": {"$set": {"a": 1}}, "upsert": True}],
                    "writeConcern": {"w": 2, "wtimeout": 50},
                },
            )
        ],
    )
    # Validation failure on every write path that can hit it.
    vs = {"$jsonSchema": S}
    seed = ("insert", {"documents": [{"_id": 1, "a": 9}, {"_id": 2, "a": 8}]})

    def wpath(c, *steps):
        return [
            ("create", {"create": c, "validator": vs}),
            (seed[0], {"insert": c, **{k: v for k, v in seed[1].items() if k != "insert"}}),
            *steps,
        ]

    yield (
        "validation update replacement",
        wpath("v1", ("update", {"update": "v1", "updates": [{"q": {"_id": 1}, "u": {"a": 1}}]})),
    )
    yield (
        "validation update multi",
        wpath(
            "v2",
            (
                "update",
                {"update": "v2", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "multi": True}]},
            ),
        ),
    )
    yield (
        "validation update pipeline",
        wpath(
            "v3",
            ("update", {"update": "v3", "updates": [{"q": {"_id": 1}, "u": [{"$set": {"a": 1}}]}]}),
        ),
    )
    yield (
        "validation upsert insert",
        wpath(
            "v4",
            (
                "update",
                {
                    "update": "v4",
                    "updates": [{"q": {"_id": 5}, "u": {"$set": {"a": 1}}, "upsert": True}],
                },
            ),
        ),
    )
    yield (
        "validation update ordered:false",
        wpath(
            "v5",
            (
                "update",
                {
                    "update": "v5",
                    "ordered": False,
                    "updates": [
                        {"q": {"_id": 1}, "u": {"$set": {"a": 1}}},
                        {"q": {"_id": 2}, "u": {"$set": {"a": 10}}},
                    ],
                },
            ),
        ),
    )
    yield (
        "validation findAndModify update",
        wpath(
            "v6",
            (
                "findAndModify",
                {"findAndModify": "v6", "query": {"_id": 1}, "update": {"$set": {"a": 1}}},
            ),
        ),
    )
    yield (
        "validation findAndModify replace",
        wpath(
            "v7",
            ("findAndModify", {"findAndModify": "v7", "query": {"_id": 1}, "update": {"a": 1}}),
        ),
    )
    yield (
        "validation findAndModify pipeline",
        wpath(
            "v8",
            (
                "findAndModify",
                {"findAndModify": "v8", "query": {"_id": 1}, "update": [{"$set": {"a": 1}}]},
            ),
        ),
    )
    yield (
        "validation findAndModify upsert",
        wpath(
            "v9",
            (
                "findAndModify",
                {
                    "findAndModify": "v9",
                    "query": {"_id": 7},
                    "update": {"$set": {"a": 1}},
                    "upsert": True,
                },
            ),
        ),
    )
    yield (
        "validation insert ordered:false",
        wpath(
            "v10",
            (
                "insert",
                {
                    "insert": "v10",
                    "ordered": False,
                    "documents": [{"_id": 3, "a": 1}, {"_id": 4, "a": 7}],
                },
            ),
        ),
    )
    # Validator parse errors on create / collMod.
    yield (
        "create validator integer nested",
        [
            (
                "create",
                {"create": "cv1", "validator": {"$and": [{"$jsonSchema": {"type": "integer"}}]}},
            )
        ],
    )
    yield (
        "create validator unknown keyword",
        [("create", {"create": "cv2", "validator": {"$jsonSchema": {"nope": 1}}})],
    )
    yield (
        "create validator bad operator",
        [("create", {"create": "cv3", "validator": {"a": {"$nope": 1}}})],
    )
    yield (
        "collMod validator integer",
        [
            ("create", {"create": "cv4"}),
            ("collMod", {"collMod": "cv4", "validator": {"$jsonSchema": {"type": "integer"}}}),
        ],
    )
    yield (
        "create existing validator integer",
        [
            ("create", {"create": "cv5"}),
            ("create", {"create": "cv5", "validator": {"$jsonSchema": {"type": "integer"}}}),
        ],
    )
    # writeConcern.w of each wrong type.
    for name, w in [
        ("bool", True),
        ("double", 1.5),
        ("null", None),
        ("object", {"dc1": 1}),
        ("negative", -1),
        ("double 2.5", 2.5),
        ("double -1.5", -1.5),
        ("decimal 3", D("3")),
        ("51", 51),
        ("empty object", {}),
        ("two tags", {"dc1": 1, "rack": 2}),
    ]:
        yield (
            f"write concern w {name}",
            [
                (
                    "insert",
                    {"insert": f"wc_{name}", "documents": [{"_id": 1}], "writeConcern": {"w": w}},
                )
            ],
        )
    yield (
        "bucketAuto decimal powers of two",
        [
            (
                "insert",
                {
                    "insert": "ba2",
                    "documents": [{"_id": i, "v": D(str(i * 1.5))} for i in range(1, 9)],
                },
            ),
            (
                "aggregate",
                {
                    "aggregate": "ba2",
                    "pipeline": [
                        {"$bucketAuto": {"groupBy": "$v", "buckets": 3, "granularity": "POWERSOF2"}}
                    ],
                    "cursor": {},
                },
            ),
        ],
    )
    for name, arr, q in [
        ("scalar eq", [1, 2, 3], {"a": 2}),
        ("scalar gt", [1, 2, 3], {"a": {"$gt": 1}}),
        ("elemMatch scalar", [1, 2, 3], {"a": {"$elemMatch": {"$gte": 3}}}),
        ("elemMatch doc", [{"k": 1}, {"k": 2}], {"a": {"$elemMatch": {"k": 2}}}),
        ("dotted", [{"k": 1}, {"k": 2}], {"a.k": 2}),
    ]:
        c = "pos_" + name.replace(" ", "_")
        yield (
            f"positional update {name}",
            [
                ("insert", {"insert": c, "documents": [{"_id": 1, "a": arr}]}),
                ("update", {"update": c, "updates": [{"q": q, "u": {"$set": {"a.$": 9}}}]}),
                ("find", {"find": c}),
            ],
        )
    yield (
        "create existing with different options",
        [
            ("create", {"create": "cx1", "capped": True, "size": 4096}),
            ("create", {"create": "cx1", "validator": {"a": {"$gt": 1}}}),
        ],
    )
    yield (
        "create existing same options",
        [
            ("create", {"create": "cx2", "validator": {"a": {"$gt": 1}}}),
            ("create", {"create": "cx2", "validator": {"a": {"$gt": 1}}}),
        ],
    )
    yield (
        "project _id only",
        [
            ("insert", {"insert": "pj", "documents": [{"_id": 1, "a": 1, "b": 2}]}),
            (
                "aggregate",
                {"aggregate": "pj", "pipeline": [{"$project": {"_id": 1}}], "cursor": {}},
            ),
        ],
    )
    yield (
        "project _id:1 a:0 mixed",
        [
            (
                "aggregate",
                {"aggregate": "pj", "pipeline": [{"$project": {"_id": 1, "a": 0}}], "cursor": {}},
            )
        ],
    )
    yield (
        "project _id:0 only",
        [("aggregate", {"aggregate": "pj", "pipeline": [{"$project": {"_id": 0}}], "cursor": {}})],
    )
    yield "find projection _id only", [("find", {"find": "pj", "projection": {"_id": 1}})]
    yield (
        "positional update",
        [
            ("insert", {"insert": "pos", "documents": [{"_id": 1, "a": [1, 2, 3]}]}),
            ("update", {"update": "pos", "updates": [{"q": {"a": 2}, "u": {"$set": {"a.$": 9}}}]}),
            ("find", {"find": "pos"}),
        ],
    )
    exprs = {
        "slice neg position": {"$slice": [[1, 2, 3, 4, 5], -2, 1]},
        "slice neg position big": {"$slice": [[1, 2, 3, 4, 5], -9, 2]},
        "pow decimal": {"$pow": [D("2"), D("0.5")]},
        "pow decimal int": {"$pow": [D("1.5"), 3]},
        "pow decimal neg": {"$pow": [D("-8"), D("0.3333333333333333333333333333333333")]},
        "exp decimal": {"$exp": D("1")},
        "sqrt decimal": {"$sqrt": D("2")},
        "log decimal": {"$log": [D("100"), D("10")]},
        "round decimal": {"$round": [D("2.555"), 2]},
        "trunc decimal": {"$trunc": [D("-2.555"), 1]},
        "mod decimal": {"$mod": [D("7.5"), 2]},
        "abs decimal": {"$abs": D("-1.5")},
        "sin decimal": {"$sin": D("1")},
        "toInt decimal": {"$toInt": D("7.9")},
        "range decimal": {"$range": [0, D("3")]},
        "arrayElemAt decimal": {"$arrayElemAt": [[1, 2, 3], D("1")]},
        "substrCP decimal": {"$substrCP": ["hello", D("1"), D("2")]},
        "ln decimal neg": {"$ln": D("-1")},
        "slice count zero": {"$slice": [[1, 2, 3], 1, 0]},
        "slice count negative": {"$slice": [[1, 2, 3], 1, -1]},
        "slice neg position past end": {"$slice": [[1, 2, 3, 4, 5], -1, 5]},
        "range decimal start": {"$range": [D("1"), 4]},
        "range decimal step": {"$range": [0, 6, D("2")]},
        "range decimal fractional start": {"$range": [D("1.5"), 4]},
        "pow decimal exact square": {"$pow": [D("2.5"), 2]},
        # $dateToString directives beyond the numeric subset.
        "dateToString %z": {"$dateToString": {"date": DT, "format": "%z", "timezone": "-0530"}},
        "dateToString %Z": {"$dateToString": {"date": DT, "format": "%Z", "timezone": "+0230"}},
        "dateToString iso week": {"$dateToString": {"date": DT, "format": "%G-W%V-%u"}},
        "dateToString %U %w %j": {"$dateToString": {"date": DT, "format": "%U %w %j"}},
        "dateToString %b %B %a": {"$dateToString": {"date": DT, "format": "%b %B %a"}},
        "dateToString named zone %z": {
            "$dateToString": {
                "date": DT,
                "format": "%Y-%m-%d %H:%M %z",
                "timezone": "America/New_York",
            }
        },
        # The timezone form of $dateTrunc / $dateDiff, across a DST change.
        "dateTrunc day tz": {
            "$dateTrunc": {"date": DT, "unit": "day", "timezone": "America/New_York"}
        },
        "dateTrunc hour tz": {
            "$dateTrunc": {"date": DT, "unit": "hour", "timezone": "America/New_York"}
        },
        "dateTrunc week tz": {
            "$dateTrunc": {
                "date": DT,
                "unit": "week",
                "timezone": "Europe/London",
                "startOfWeek": "mon",
            }
        },
        "dateTrunc week mon no tz": {
            "$dateTrunc": {"date": DT, "unit": "week", "startOfWeek": "mon"}
        },
        "dateTrunc week MONDAY no tz": {
            "$dateTrunc": {"date": DT, "unit": "week", "startOfWeek": "MONDAY"}
        },
        "dateTrunc week default tz": {
            "$dateTrunc": {"date": DT, "unit": "week", "timezone": "America/New_York"}
        },
        "dateTrunc week bad start": {
            "$dateTrunc": {"date": DT, "unit": "week", "startOfWeek": "mo"}
        },
        "dateTrunc month tz": {
            "$dateTrunc": {"date": DT, "unit": "month", "timezone": "Asia/Tokyo"}
        },
        "dateTrunc quarter tz": {
            "$dateTrunc": {"date": DT, "unit": "quarter", "timezone": "+05:30"}
        },
        "dateTrunc year tz bin": {
            "$dateTrunc": {"date": DT, "unit": "year", "binSize": 2, "timezone": "Europe/Paris"}
        },
        "dateTrunc hour bin tz": {
            "$dateTrunc": {"date": DT, "unit": "hour", "binSize": 5, "timezone": "America/New_York"}
        },
        "dateDiff month tz": {
            "$dateDiff": {
                "startDate": {"$toDate": "2024-01-31T23:30:00Z"},
                "endDate": DT,
                "unit": "month",
                "timezone": "Asia/Tokyo",
            }
        },
        "dateDiff week startOfWeek": {
            "$dateDiff": {
                "startDate": {"$toDate": "2024-03-01T12:00:00Z"},
                "endDate": DT,
                "unit": "week",
                "startOfWeek": "fri",
            }
        },
        "dateDiff day tz": {
            "$dateDiff": {
                "startDate": {"$toDate": "2024-03-09T12:00:00Z"},
                "endDate": DT,
                "unit": "day",
                "timezone": "America/New_York",
            }
        },
        "dateDiff day no tz": {
            "$dateDiff": {
                "startDate": {"$toDate": "2024-03-09T23:00:00Z"},
                "endDate": DT,
                "unit": "day",
            }
        },
        "dateDiff week tz": {
            "$dateDiff": {
                "startDate": {"$toDate": "2024-03-01T12:00:00Z"},
                "endDate": DT,
                "unit": "week",
                "timezone": "America/New_York",
            }
        },
        # Decimal transcendentals and conversions.
        "ln decimal": {"$ln": D("10")},
        "log10 decimal": {"$log10": D("1000")},
        "cos decimal": {"$cos": D("1")},
        "tan decimal": {"$tan": D("0.5")},
        "asin decimal": {"$asin": D("0.5")},
        "atanh decimal": {"$atanh": D("0.5")},
        "sinh decimal": {"$sinh": D("1")},
        "toDate decimal": {"$toDate": D("1700000000000")},
        "toDate decimal fraction": {"$toDate": D("1700000000000.7")},
        "log decimal base int": {"$log": [D("8"), 2]},
        "log int base decimal": {"$log": [8, D("2")]},
        "log decimal double": {"$log": [D("10"), 2.5]},
        "log decimal exact": {"$log": [D("1000"), D("10")]},
        "log decimal nan": {"$log": [D("NaN"), D("10")]},
        "log decimal inf": {"$log": [D("Infinity"), D("10")]},
        "log decimal base 1": {"$log": [D("10"), D("1")]},
        "log decimal zero": {"$log": [D("0"), D("10")]},
    }
    for k, e in exprs.items():
        yield (
            f"expr {k}",
            [
                (
                    "aggregate",
                    {
                        "aggregate": "one",
                        "pipeline": [{"$project": {"_id": 0, "r": e}}],
                        "cursor": {},
                    },
                )
            ],
        )
    yield (
        "bucketAuto decimal granularity",
        [
            (
                "insert",
                {
                    "insert": "ba",
                    "documents": [{"_id": i, "v": D(str(i * 1.5))} for i in range(1, 9)],
                },
            ),
            (
                "aggregate",
                {
                    "aggregate": "ba",
                    "pipeline": [
                        {"$bucketAuto": {"groupBy": "$v", "buckets": 3, "granularity": "R5"}}
                    ],
                    "cursor": {},
                },
            ),
        ],
    )
    yield (
        "bucketAuto decimal plain",
        [
            (
                "aggregate",
                {
                    "aggregate": "ba",
                    "pipeline": [{"$bucketAuto": {"groupBy": "$v", "buckets": 3}}],
                    "cursor": {},
                },
            )
        ],
    )


DT = {"$toDate": "2024-03-10T06:30:45.123Z"}
STRIP = {
    "$clusterTime",
    "operationTime",
    "electionId",
    "opTime",
    "lastCommittedOpTime",
    "$configTime",
    "$topologyTime",
}


UUID_RE = re.compile(r'UUID\("[0-9a-f-]{36}"\)')


def clean(r):
    if isinstance(r, str):
        return UUID_RE.sub('UUID("<uuid>")', r)
    if isinstance(r, dict):
        return {k: clean(v) for k, v in r.items() if k not in STRIP}
    if isinstance(r, list):
        return [clean(v) for v in r]
    return r


def run(uri):
    c = pymongo.MongoClient(uri)
    c.drop_database("p0m")
    db = c.get_database("p0m", codec_options=CO)
    db.one.insert_one({"_id": 1})
    out = []
    for name, steps in cases():
        res = []
        for _, cmd in steps:
            try:
                r = db.command(cmd)
            except pymongo.errors.OperationFailure as e:
                r = e.details
            if "cursor" in r:
                r = {"ok": r.get("ok"), "batch": r["cursor"]["firstBatch"]}
            res.append(clean(norm(r)))
        out.append((name, res))
    c.drop_database("p0m")
    return out


a, b = run(sys.argv[1]), run(sys.argv[2])
bad = 0
for (n, x), (_, y) in zip(a, b, strict=True):
    if x != y:
        bad += 1
        print(f"DIFF {n}")
        for i, (p, q) in enumerate(zip(x, y, strict=True)):
            if p != q:
                want, got = json.dumps(p)[:600], json.dumps(q)[:600]
                print(f"  step {i}\n    mongod {want}\n    rust   {got}")
print(f"=== misc: {bad} of {len(a)} divergent ===")
