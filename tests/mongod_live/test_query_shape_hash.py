"""`queryShapeHash` against mongod 8.2 (RM-358): the SHA-256 of a `find`'s,
`distinct`'s or `aggregate`'s representative query shape, which mongod
prints on its `Slow query` line, its profile entry and its `explain`.

Each case runs `explain` of the same command on both servers, in a database
and collection of the same names (the hash covers the namespace), and
compares the hashes. The cases sweep what the shape is made of: the filter
as mongod parses it (not as it optimizes it), the literal each value stands
for, the projection, the sort and the options, and the pipeline stages a
command is parsed into. Geo predicates, `$text`, the aggregation operators
and accumulators with arguments of their own, and the stages mongod
desugars (`$bucket`, `$setWindowFields`, `$fill`, `$densify`, `$documents`,
`$score`, ...) each have their own serializer, measured against mongod
8.2.11 on 2026-11-09; `$rankFusion` and `$scoreFusion` are the pipelines
mongod desugars them into (measured 2026-11-12).
"""

from __future__ import annotations

import datetime
import struct
from typing import Any

import pytest
from bson import Binary, Decimal128, Int64, ObjectId, Regex, Timestamp
from bson.raw_bson import RawBSONDocument
from bson.son import SON

_DB = "qshape_live"

FILTERS: list[Any] = [
    {},
    {"x": 1},
    SON([("y", 5), ("x", 2)]),
    {"x": {"$gt": 1, "$lt": 5}},
    {"x": {"$in": [3, "a", 1]}},
    {"x": {"$in": [1, Int64(2)]}},
    {"x": {"$in": [1.5, 2]}},
    {"x": {"$in": []}},
    {"x": {"$in": [5]}},
    {"x": {"$in": [Regex("a")]}},
    {"x": {"$in": [1, Regex("a")]}},
    {"x": {"$in": [[1], [2]]}},
    {"x": {"$in": [None]}},
    {"x": {"$eq": [1, Int64(2)]}},
    {"x": {"$eq": Regex("a")}},
    {"x": Regex("a")},
    {"x": Regex("a", "i")},
    {"x": {"$regex": "a", "$options": "si"}},
    {"x": {"$options": "i", "$regex": "a"}},
    {"x": {"$regex": Regex("a", "i")}},
    {"x": {"$not": {"$gt": 1, "$lt": 5}}},
    {"x": {"$not": {"$gt": 1}}},
    {"x": {"$not": Regex("a")}},
    {"$or": [{"x": 1}, {"x": 2}]},
    {"$or": [{"x": 1}, {"y": 2}]},
    {"$and": [{"x": 1}]},
    {"$nor": [{"x": 1}, {"y": {"$lt": 3}}]},
    {"$or": [{"x": 1, "y": 2}, {"z": 3}]},
    {"x": {"$elemMatch": {"$gt": 1, "$lt": 5}}},
    {"x": {"$elemMatch": {"a": 1, "b": 2}}},
    {"x": {"$elemMatch": {"a": 1}}},
    {"x": {"$elemMatch": {"$not": {"$gt": 1}}}},
    {"x": {"$elemMatch": {"$and": [{"a": 1}]}}},
    {"x": {"$all": [1, 2]}},
    {"x": {"$all": [1]}},
    {"x": {"$all": []}},
    {"x": {"$all": [{"$elemMatch": {"a": 1}}]}},
    {"x": {"$type": "number"}},
    {"x": {"$type": ["string", 1, "number"]}},
    {"x": {"$type": [16, 2]}},
    {"x": {"$bitsAllSet": 5}},
    {"x": {"$bitsAnySet": [1, 3]}},
    {"x": {"$bitsAllClear": 0}},
    {"x": {"$bitsAnyClear": Binary(b"\x01")}},
    {"x": {"$exists": True}},
    {"x": {"$exists": 0}},
    {"x": None},
    {"x": {"$ne": None}},
    {"x": {"$ne": 3}},
    {"x": {"$nin": [3, 4]}},
    {"x": datetime.datetime(2020, 1, 1)},
    {"x": ObjectId("65a000000000000000000000")},
    {"x": Decimal128("1.5")},
    {"x": True},
    {"x": "s"},
    {"x": {"a": 1, "b": 2}},
    {"x": [1, 2]},
    {"x": [1, "a"]},
    {"x": [[1, 2]]},
    {"x": []},
    {"x.y": {"$size": 2}},
    {"$comment": "hi", "x": 1},
    {"$alwaysTrue": 1},
    {"$alwaysFalse": 1},
    {"x": {"$mod": [3, 0]}},
    {"x": {"$not": {"$elemMatch": {"a": 1}}}},
    {"x": {"$not": {"$exists": True}}},
    {"x": {"$gte": Int64(5)}},
    {"$expr": {"$gt": ["$x", 2]}},
    {"$expr": {"$eq": ["$x", {"$add": ["$y", 1]}]}},
]

FIND_OPTIONS: list[dict] = [
    {"projection": {"x": 1}},
    {"projection": {"x": 0}},
    {"projection": SON([("x", 1), ("_id", 0)])},
    {"projection": SON([("_id", 1), ("x", 1)])},
    {"projection": {"a.b": 1, "a.c": 1}},
    {"projection": {"y": "$x"}},
    {"projection": {"y": "lit"}},
    {"projection": {"y": {"$add": ["$x", 1]}}},
    {"projection": {"y": {"$slice": 3}}},
    {"projection": {"y": {"$slice": [1, 3]}}},
    {"projection": {"y": {"$elemMatch": {"a": 1}}}},
    {"projection": {}},
    {"sort": {"x": -1}},
    {"sort": SON([("x", 1), ("y", -1)])},
    {"sort": {"x": 1.0}},
    {"limit": 3},
    {"skip": 3},
    {"limit": 3, "skip": 2},
    {"singleBatch": True},
    {"singleBatch": False},
    {"allowDiskUse": True},
    {"returnKey": True},
    {"showRecordId": False},
    {"batchSize": 2},
    {"collation": {"locale": "fr"}},
    {"let": {"v": 5, "w": {"$add": [1, 2]}, "s": {"$literal": "a"}}},
    {"hint": {"x": 1}, "min": {"x": 2}, "max": {"x": 8}},
]

PIPELINES: list[list] = [
    [],
    [{"$match": {"x": {"$gt": 1}}}, {"$sort": {"x": -1}}, {"$limit": 3}, {"$skip": 1}],
    [{"$count": "n"}],
    [
        {
            "$group": {
                "_id": "$y",
                "n": {"$sum": 1},
                "t": {"$sum": "$x"},
                "m": {"$max": "$x"},
                "lo": {"$min": "$x"},
                "p": {"$push": "$x"},
                "f": {"$first": "$x"},
                "l": {"$last": "$x"},
            }
        }
    ],
    [{"$group": {"_id": 1, "n": {"$sum": 1}}}],
    [{"$group": {"_id": {"a": "$y", "b": "$x"}, "c": {"$count": {}}}}],
    [{"$group": {"_id": None, "c": {"$avg": "$x"}, "s": {"$addToSet": "$y"}}}],
    [{"$match": {"x": 1}}, {"$group": {"_id": 1, "n": {"$sum": 1}}}],
    [{"$project": {"x": 1, "z": {"$add": ["$x", 1]}, "w": "lit", "q": {"$literal": 5}}}],
    [{"$project": {"x": 0}}],
    [{"$project": SON([("x", 1), ("_id", 0)])}],
    [{"$project": {"a.b": 1, "x": "$y"}}],
    [
        {
            "$project": {
                "c": {"$cond": [{"$gt": ["$x", 1]}, "a", "b"]},
                "s": {"$size": "$a"},
                "e": {"$eq": ["$x", 1]},
            }
        }
    ],
    [{"$addFields": {"z": {"$multiply": ["$x", 2]}, "o": {"k": 1}, "arr": [1, 2]}}],
    [{"$addFields": {"k": {"$concat": ["a", "$y"]}, "l": {"$literal": [1]}}}],
    [{"$set": {"z": "$x"}}],
    [{"$unset": "x"}],
    [{"$unset": ["x", "y"]}],
    [{"$unwind": "$a"}],
    [{"$unwind": {"path": "$a", "includeArrayIndex": "i", "preserveNullAndEmptyArrays": True}}],
    [{"$unwind": {"path": "$a", "preserveNullAndEmptyArrays": False}}],
    [{"$replaceRoot": {"newRoot": {"v": "$x"}}}],
    [{"$replaceWith": "$$ROOT"}],
    [{"$lookup": {"from": "o", "localField": "x", "foreignField": "k", "as": "j"}}],
    [{"$sample": {"size": 2}}],
    [{"$sortByCount": "$y"}],
    [{"$match": {"$expr": {"$gt": ["$x", 2]}}}],
]

_D = datetime.datetime(2020, 1, 1)

#: Aggregation expressions, each the value of a `$project`ed field.
EXPRESSIONS: list[Any] = [
    {"$dateToString": {"date": "$t", "format": "%Y"}},
    SON(
        [
            (
                "$dateToString",
                SON([("onNull", "n"), ("timezone", "UTC"), ("format", "%Y"), ("date", "$t")]),
            )
        ]
    ),
    {"$dateFromString": {"dateString": "$s", "onError": 1}},
    SON([("$dateFromParts", SON([("hour", 1), ("isoWeekYear", 2020), ("isoWeek", 2)]))]),
    {"$dateFromParts": {"year": 2020, "month": "$m"}},
    {"$dateToParts": {"date": "$t", "iso8601": True}},
    {"$year": "$t"},
    {"$isoWeek": ["$t"]},
    {"$dayOfWeek": {"date": "$t", "timezone": "UTC"}},
    {"$dateAdd": {"startDate": _D, "unit": "$u", "amount": {"$add": ["$x", 1]}}},
    {"$dateDiff": {"startDate": "$t", "endDate": "$u", "unit": "week", "startOfWeek": "mon"}},
    {"$dateTrunc": {"date": "$t", "unit": "day", "binSize": 2}},
    {"$convert": {"input": "$x", "to": "int", "onError": 0, "onNull": 1}},
    {"$convert": {"input": "$x", "to": 2}},
    {"$convert": {"input": "$x", "to": {"type": "binData", "subtype": 0}}},
    {"$toString": "$x"},
    {"$toInt": ["$x"]},
    {"$toUUID": "$x"},
    {"$let": {"vars": {"a": "$x", "b": 1}, "in": {"$add": ["$$a", "$$b"]}}},
    {"$map": {"input": "$a", "in": "$$this"}},
    {"$filter": {"input": "$a", "as": "e", "cond": {"$gt": ["$$e", 1]}, "limit": 2}},
    {
        "$reduce": {
            "input": "$a",
            "initialValue": [],
            "in": {"$concatArrays": ["$$value", ["$$this"]]},
        }
    },
    {"$switch": {"branches": [{"case": {"$gt": ["$x", 1]}, "then": "a"}], "default": "b"}},
    {"$cond": {"if": "$b", "then": 1, "else": "$x"}},
    {"$regexMatch": {"input": "$s", "regex": Regex("a"), "options": "i"}},
    {"$regexFindAll": {"input": "$s", "regex": "a"}},
    {"$rtrim": {"chars": "x", "input": "$s"}},
    {"$getField": "a"},
    {"$getField": {"field": {"$literal": "$a"}, "input": "$o"}},
    {"$setField": {"field": "a", "input": "$$ROOT", "value": 1}},
    {"$unsetField": {"field": "a", "input": "$$ROOT"}},
    {"$sortArray": {"input": "$a", "sortBy": {"k": -1}}},
    {"$zip": {"inputs": ["$a", [1, 2]], "useLongestLength": True, "defaults": [0, 0]}},
    {"$meta": "indexKey"},
    {"$rand": {}},
    {"$replaceAll": {"input": "$s", "find": "a", "replacement": "$r"}},
    {"$mergeObjects": ["$a", {"k": 1}]},
    {"$maxN": {"n": 2, "input": "$a"}},
    {"$median": {"input": "$a", "method": "approximate"}},
    {"$percentile": {"input": "$a", "p": [0.5, 0.9], "method": "approximate"}},
    {"$substr": ["$s", 0, 1]},
    {"$sin": "$x"},
    {"$bitAnd": ["$x", 1]},
    {"$toHashedIndexKey": "$x"},
    {"$sigmoid": "$x"},
    {"$similarityCosine": ["$a", "$b"]},
    {"$similarityEuclidean": {"vectors": [[1, 2], "$v"], "score": True}},
]

#: Accumulators, stages and the stages mongod desugars.
STAGES: list[list] = [
    [{"$group": {"_id": "$y", "r": {"$topN": {"n": 2, "output": "$x", "sortBy": {"x": 1}}}}}],
    [{"$group": {"_id": "$y", "r": {"$bottom": {"sortBy": {"x": -1}, "output": ["$x", "$y"]}}}}],
    [
        {
            "$group": {
                "_id": "$y",
                "r": {"$lastN": {"n": 2, "input": "$x"}},
                "m": {"$mergeObjects": "$o"},
            }
        }
    ],
    [
        {
            "$group": {
                "_id": None,
                "m": {"$percentile": {"input": "$x", "p": [0.5], "method": "approximate"}},
            }
        }
    ],
    [
        {
            "$group": {
                "_id": "$y",
                "r": {
                    "$accumulator": {
                        "init": "function(){return 0}",
                        "accumulate": "function(s,x){return s+x}",
                        "accumulateArgs": ["$x"],
                        "merge": "function(a,b){return a+b}",
                        "lang": "js",
                    }
                },
            }
        }
    ],
    [{"$facet": {"a": [{"$match": {"x": 1}}], "b": [{"$count": "n"}]}}],
    [{"$lookup": {"from": "c", "pipeline": [{"$match": {"x": 1}}], "as": "j"}}],
    [
        {
            "$lookup": {
                "from": "c",
                "let": {"v": "$x", "k": 1},
                "pipeline": [{"$limit": 1}],
                "as": "j",
            }
        }
    ],
    [{"$lookup": {"from": "o", "localField": "x", "foreignField": "k", "pipeline": [], "as": "j"}}],
    [
        {
            "$lookup": {
                "from": "o",
                "localField": "x",
                "foreignField": "k",
                "pipeline": [{"$limit": 1}],
                "as": "j",
            }
        }
    ],
    [{"$bucket": {"groupBy": "$x", "boundaries": [0, 5, 10], "default": "o"}}],
    [
        {
            "$bucket": {
                "groupBy": "$x",
                "boundaries": [0, 2],
                "output": {"n": {"$sum": 1}, "m": {"$max": "$y"}},
            }
        }
    ],
    [{"$bucketAuto": {"groupBy": "$x", "buckets": 3, "granularity": "R5"}}],
    [
        {
            "$graphLookup": {
                "from": "c",
                "startWith": "$x",
                "connectFromField": "x",
                "connectToField": "y",
                "as": "g",
                "maxDepth": 2,
                "depthField": "d",
                "restrictSearchWithMatch": {"z": 1},
            }
        }
    ],
    [{"$unionWith": "o"}],
    [{"$unionWith": {"coll": "o", "pipeline": [{"$match": {"k": 1}}]}}],
    [{"$redact": {"$cond": [{"$eq": ["$x", 1]}, "$$PRUNE", "$$DESCEND"]}}],
    [
        {
            "$densify": {
                "field": "x",
                "partitionByFields": ["y"],
                "range": {"step": 1, "bounds": "partition"},
            }
        }
    ],
    [{"$densify": {"field": "x", "range": {"step": 1, "bounds": [0, 10]}}}],
    [{"$fill": {"output": {"x": {"value": 0}}}}],
    [
        {
            "$fill": {
                "sortBy": {"x": 1},
                "partitionByFields": ["y"],
                "output": {"z": {"method": "locf"}},
            }
        }
    ],
    [{"$fill": {"sortBy": {"x": 1}, "output": {"z": {"method": "linear"}, "w": {"value": "a"}}}}],
    [
        {
            "$setWindowFields": {
                "partitionBy": "$y",
                "sortBy": {"x": 1},
                "output": {"s": {"$sum": "$x", "window": {"documents": [-1, 1]}}},
            }
        }
    ],
    [{"$setWindowFields": {"partitionBy": {"$mod": ["$x", 2]}, "output": {"s": {"$push": "$x"}}}}],
    [
        {
            "$setWindowFields": {
                "sortBy": {"x": 1},
                "output": {
                    "n": {"$documentNumber": {}},
                    "p": {"$shift": {"output": "$x", "by": 1}},
                    "e": {"$expMovingAvg": {"input": "$x", "alpha": 0.5}},
                },
            }
        }
    ],
    [{"$out": "outc"}],
    [{"$merge": {"into": "mc", "let": {"v": 1}, "whenMatched": [{"$set": {"z": "$$v"}}]}}],
    [{"$score": {"score": "$x", "normalization": "sigmoid", "weight": 0.5}}],
    [{"$collStats": {"storageStats": {"scale": 1024}, "count": {}}}],
    [{"$indexStats": {}}],
    [{"$project": SON([("z", "$y"), ("a", SON([("q", 1)])), ("x", 1)])}],
    [{"$project": SON([("a.b", 0), ("x", 0)])}],
]

#: Geo predicates (over `geo`, which has a 2d and a 2dsphere index), and
#: the pathless predicates with a form of their own. (`$where`, `$function`,
#: `$text` and a `$meta` sort, which this server refuses to run, are pinned
#: against mongod's hashes in `query_shape.rs`'s unit tests.)
GEO_FILTERS: list[Any] = [
    {"loc": {"$geoWithin": {"$center": [[0, 0], 5]}}},
    {"loc": {"$geoWithin": {"$centerSphere": [{"a": 0, "b": 0}, 0.1]}}},
    {"loc": {"$geoWithin": {"$box": [[0, 0], [5, 5]]}}},
    {"loc": {"$within": {"$polygon": [[0, 0], [5, 5], [0, 5]]}}},
    {
        "g": {
            "$geoWithin": {
                "$geometry": {"type": "Polygon", "coordinates": [[[0, 0], [5, 0], [5, 5], [0, 0]]]}
            }
        }
    },
    {
        "g": {
            "$geoIntersects": {
                "$geometry": {"type": "LineString", "coordinates": [[0, 0], [3, 3], [5, 1]]}
            }
        }
    },
    {
        "g": {
            "$geoIntersects": {
                "$geometry": {"type": "MultiLineString", "coordinates": [[[0, 0], [3, 3]]]}
            }
        }
    },
    {
        "g": {
            "$geoIntersects": {"$geometry": {"type": "MultiPoint", "coordinates": [[0, 0], [2, 2]]}}
        }
    },
    {
        "g": {
            "$geoIntersects": {
                "$geometry": {
                    "type": "GeometryCollection",
                    "geometries": [{"type": "Point", "coordinates": [0, 0]}],
                }
            }
        }
    },
    {"loc": {"$near": [0, 0], "$maxDistance": 5}},
    {
        "g": {
            "$nearSphere": {
                "$geometry": {"type": "Point", "coordinates": [0, 0]},
                "$minDistance": 1,
            }
        }
    },
    {"loc": {"$not": {"$geoWithin": {"$center": [[0, 0], 1]}}}},
    {"$sampleRate": 0.5},
]


def _dbpointer(field: str) -> RawBSONDocument:
    """`{field: DBPointer("a.b", <oid>)}`, which pymongo cannot build."""
    element = (
        b"\x0c" + field.encode() + b"\x00" + struct.pack("<i", 4) + b"a.b\x00" + bytes(range(12))
    )
    return RawBSONDocument(struct.pack("<i", len(element) + 5) + element + b"\x00")


def _rank_fusion(pipelines: dict, **options: Any) -> dict:
    return {"$rankFusion": {"input": {"pipelines": pipelines}, **options}}


def _score_fusion(pipelines: dict, normalization: str, **options: Any) -> dict:
    spec = {"input": {"pipelines": pipelines, "normalization": normalization}, **options}
    return {"$scoreFusion": spec}


_RANKED = {"a": [{"$sort": {"x": 1}}], "b": [{"$sort": {"x": -1}}]}
_SCORED = {"a": [{"$score": {"score": "$x"}}], "b": [{"$score": {"score": {"$add": ["$x", 1]}}}]}

#: `$rankFusion` and `$scoreFusion`, which mongod shapes as the pipelines it
#: desugars them into (`document_source_rank_fusion.cpp`,
#: `document_source_score_fusion.cpp`), each with the collection it runs
#: against. Pinned instead against mongod's hashes in `secantus-commands`'
#: `query_shape.rs`, as this server cannot explain them yet: an input of
#: `$text` (no text index), of `$geoNear` (RM-552) and a
#: `combination.method` of `"avg"` (RM-553).
HYBRID: list[tuple[str, list]] = [
    ("c", [_rank_fusion(_RANKED)]),
    ("c", [_rank_fusion(_RANKED, scoreDetails=True)]),
    ("c", [_rank_fusion(_RANKED, combination={"weights": {"b": 2}})]),
    (
        "c",
        [
            _rank_fusion(
                SON(
                    [
                        ("b", [{"$match": {"x": {"$gt": 0}}}, {"$sort": {"x": 1}}, {"$limit": 5}]),
                        ("a", [{"$sort": {"x": -1}}, {"$skip": 1}]),
                        ("Z", [{"$sort": {"y": 1, "x": -1}}]),
                    ]
                )
            )
        ],
    ),
    ("c", [_score_fusion(_SCORED, "none")]),
    ("c", [_score_fusion(_SCORED, "sigmoid")]),
    ("c", [_score_fusion({"a": [{"$score": {"score": "$x"}}]}, "minMaxScaler")]),
    ("c", [_score_fusion(_SCORED, "none", combination={"weights": {"b": 0.5}})]),
    (
        "c",
        [
            _score_fusion(
                _SCORED,
                "none",
                combination={
                    "method": "expression",
                    "expression": {"$add": ["$$a", {"$multiply": ["$$b", 2]}]},
                },
            )
        ],
    ),
    (
        "c",
        [
            _score_fusion(
                {
                    "a": [{"$score": {"score": "$x"}}],
                    "b": [
                        {"$match": {"x": 1}},
                        {"$score": {"score": "$x", "normalization": "sigmoid", "weight": 0.5}},
                    ],
                },
                "minMaxScaler",
                scoreDetails=True,
            )
        ],
    ),
    # Nested, a hybrid search runs against the collection of the stage
    # holding it; a `$lookup` of one carries a flag the hash covers.
    ("c", [{"$unionWith": {"coll": "o", "pipeline": [_rank_fusion(_RANKED)]}}]),
    ("c", [{"$lookup": {"from": "o", "as": "j", "pipeline": [_rank_fusion(_RANKED)]}}]),
    ("c", [{"$lookup": {"from": "o", "as": "j", "pipeline": [_score_fusion(_SCORED, "none")]}}]),
    ("c", [{"$facet": {"f": [_rank_fusion(_RANKED)]}}]),
]


DISTINCTS: list[tuple[str, Any]] = [
    ("x", None),
    ("x", {"x": {"$gt": 5}}),
    ("y.z", SON([("y", 5), ("x", {"$in": [1, "a"]})])),
]


@pytest.fixture(scope="module")
def servers(rust_mongo: Any, mongod_rs: Any) -> Any:
    for client in (rust_mongo, mongod_rs):
        db = client[_DB]
        db.c.drop()
        db.o.drop()
        db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "a": [1, 2]} for n in range(6)])
        db.c.create_index("x")
        db.o.insert_one({"k": 1})
        db.geo.insert_one({"loc": [0, 0], "g": {"type": "Point", "coordinates": [0, 0]}})
        db.geo.create_index([("loc", "2d")])
        db.geo.create_index([("g", "2dsphere")])
    yield rust_mongo, mongod_rs
    for client in (rust_mongo, mongod_rs):
        client.drop_database(_DB)


def _hashes(servers: Any, command: SON) -> tuple[Any, Any]:
    out = []
    for client in servers:
        reply = client[_DB].command("explain", command, verbosity="queryPlanner")
        out.append(reply.get("queryShapeHash"))
    return out[0], out[1]


def _check(servers: Any, command: SON) -> None:
    ours, theirs = _hashes(servers, command)
    assert theirs, f"mongod printed no queryShapeHash for {command}"
    assert ours == theirs, command


@pytest.mark.parametrize("filt", FILTERS, ids=[str(f) for f in FILTERS])
def test_a_finds_filter(servers: Any, filt: Any) -> None:
    _check(servers, SON([("find", "c"), ("filter", filt)]))


@pytest.mark.parametrize("options", FIND_OPTIONS, ids=[str(o) for o in FIND_OPTIONS])
def test_a_finds_options(servers: Any, options: dict) -> None:
    command = SON([("find", "c"), ("filter", {"x": {"$gt": 1}})])
    command.update(options)
    _check(servers, command)


@pytest.mark.parametrize("pipeline", PIPELINES, ids=[str(p) for p in PIPELINES])
def test_an_aggregates_pipeline(servers: Any, pipeline: list) -> None:
    _check(servers, SON([("aggregate", "c"), ("pipeline", pipeline), ("cursor", {})]))


def test_an_aggregates_options(servers: Any) -> None:
    _check(
        servers,
        SON(
            [
                ("aggregate", "c"),
                ("pipeline", [{"$match": {"x": 1}}]),
                ("cursor", {}),
                ("collation", {"locale": "fr"}),
                ("let", {"v": 1}),
                ("allowDiskUse", True),
            ]
        ),
    )


@pytest.mark.parametrize("expression", EXPRESSIONS, ids=[str(e) for e in EXPRESSIONS])
def test_an_expression(servers: Any, expression: Any) -> None:
    pipeline = [{"$project": {"r": expression}}]
    _check(servers, SON([("aggregate", "c"), ("pipeline", pipeline), ("cursor", {})]))


@pytest.mark.parametrize("pipeline", STAGES, ids=[str(p) for p in STAGES])
def test_a_stage(servers: Any, pipeline: list) -> None:
    _check(servers, SON([("aggregate", "c"), ("pipeline", pipeline), ("cursor", {})]))


@pytest.mark.parametrize("coll,pipeline", HYBRID, ids=[str(h) for h in HYBRID])
def test_a_hybrid_search(servers: Any, coll: str, pipeline: list) -> None:
    _check(servers, SON([("aggregate", coll), ("pipeline", pipeline), ("cursor", {})]))


@pytest.mark.parametrize("filt", GEO_FILTERS, ids=[str(f) for f in GEO_FILTERS])
def test_a_geo_or_pathless_filter(servers: Any, filt: Any) -> None:
    _check(servers, SON([("find", "geo"), ("filter", filt)]))


def test_a_dbpointer_literal(servers: Any) -> None:
    _check(servers, SON([("find", "c"), ("filter", _dbpointer("x"))]))


def test_a_collectionless_aggregate(servers: Any) -> None:
    """`aggregate: 1` runs on `<db>.$cmd.aggregate`; `$documents` is the
    `$queue`, `$project`, `$unwind` and `$replaceRoot` mongod parses it into."""
    for docs in (
        [{"x": 1}, {"x": "a", "y": 2}],
        {"$map": {"input": [1, 2], "in": {"x": "$$this"}}},
    ):
        pipeline = [{"$documents": docs}, {"$match": {"x": 1}}]
        _check(servers, SON([("aggregate", 1), ("pipeline", pipeline), ("cursor", {})]))


@pytest.mark.parametrize(
    "spec",
    [
        {},
        {"fullDocument": "updateLookup", "fullDocumentBeforeChange": "whenAvailable"},
        {"startAtOperationTime": Timestamp(5, 1), "showExpandedEvents": True},
    ],
    ids=str,
)
def test_a_change_stream(servers: Any, spec: dict) -> None:
    """A change stream's shape fills in mongod's defaults, and the operation
    time it starts at."""
    pipeline = [{"$changeStream": spec}, {"$match": {"operationType": "insert"}}]
    _check(servers, SON([("aggregate", "c"), ("pipeline", pipeline), ("cursor", {})]))


def test_a_change_stream_resumed(servers: Any) -> None:
    """A resume token's representative is the high-water mark at
    `Timestamp(0, 0)`, under the key it was given. Each server resumes from
    a token of its own."""
    tokens = []
    for client in servers:
        stream = client[_DB].c.watch()
        client[_DB].c.insert_one({"_id": "resume", "x": 0})
        stream.try_next()
        tokens.append(stream.resume_token)
        stream.close()
        client[_DB].c.delete_one({"_id": "resume"})
    for key in ("resumeAfter", "startAfter"):
        hashes = []
        for client, token in zip(servers, tokens, strict=True):
            command = SON(
                [
                    ("aggregate", "c"),
                    ("pipeline", [{"$changeStream": {key: token}}]),
                    ("cursor", {}),
                ]
            )
            reply = client[_DB].command("explain", command, verbosity="queryPlanner")
            hashes.append(reply.get("queryShapeHash"))
        assert hashes[1], f"mongod printed no queryShapeHash for {key}"
        assert hashes[0] == hashes[1], key


@pytest.mark.parametrize("key,query", DISTINCTS, ids=[str(d) for d in DISTINCTS])
def test_a_distinct(servers: Any, key: str, query: Any) -> None:
    command = SON([("distinct", "c"), ("key", key)])
    if query is not None:
        command["query"] = query
    _check(servers, command)


def test_the_hash_follows_the_plan_in_explain(servers: Any) -> None:
    """mongod puts `queryShapeHash` right after the plan: `executionStats`,
    else `queryPlanner`, else a pipeline's `stages`. (Which of these an
    `aggregate` has differs where mongod runs the pipeline in SBE, an
    explain difference of its own in the backlog.)"""
    commands = [
        SON([("find", "c"), ("filter", {"x": 1})]),
        SON([("distinct", "c"), ("key", "x")]),
        SON([("aggregate", "c"), ("pipeline", [{"$group": {"_id": "$y"}}]), ("cursor", {})]),
        SON([("aggregate", "c"), ("pipeline", [{"$unwind": "$a"}]), ("cursor", {})]),
    ]
    for command in commands:
        for verbosity in ("queryPlanner", "executionStats"):
            for client in servers:
                keys = list(client[_DB].command("explain", command, verbosity=verbosity))
                plan = [k for k in ("stages", "queryPlanner", "executionStats") if k in keys]
                at = keys.index("queryShapeHash")
                assert keys[at - 1] == plan[-1], (command, verbosity, keys)


def test_a_view_an_internal_database_and_a_system_collection(servers: Any) -> None:
    """A `find` of a view hashes the view's namespace; mongod keeps no shape
    for a read of `admin`, `local` or `config`, or of a `system.`
    collection (measured 8.2.1, 2026-11-07)."""
    for client in servers:
        client[_DB].drop_collection("v")
        client[_DB].create_collection("v", viewOn="c", pipeline=[{"$match": {"x": {"$gt": 0}}}])
    _check(servers, SON([("find", "v"), ("filter", {"x": 1})]))
    for db, coll in [(_DB, "system.views"), ("admin", "c"), ("local", "c"), ("config", "c")]:
        for client in servers:
            reply = client[db].command(
                "explain", SON([("find", coll), ("filter", {"x": 1})]), verbosity="queryPlanner"
            )
            assert "queryShapeHash" not in reply, (db, coll)
