"""Differential-probe time-series collections against a real mongod.

A fresh database per scenario, each a short list of commands compared reply
for reply: what `create` accepts as a `timeseries` definition and how
`listCollections` shows it, what an insert must carry, reads and sorts, the
writes a time-series collection restricts (update, delete, findAndModify,
rename, capped, views, `$out` / `$merge`), its indexes, and `collMod`.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/time_series.py

A label ending in `~` compares a cursor's documents as an unordered bag.
"""

import datetime
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _servers import probe_targets, report  # noqa: E402
from bson import Int64, ObjectId  # noqa: E402
from pymongo.errors import PyMongoError  # noqa: E402

NOISE = (
    "$clusterTime",
    "operationTime",
    "opTime",
    "electionId",
    "uuid",
    "localTime",
    "ns",
    "info",
)
DB = "ts_probe"


def at(minute, second=0):
    start = datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc)
    return start + datetime.timedelta(minutes=minute, seconds=second)


def ts(**more):
    return {"timeField": "t", **more}


def create(name, timeseries=None, **more):
    command = {"create": name, **more}
    if timeseries is not None:
        command["timeseries"] = timeseries
    return command


def insert(name, *docs, **more):
    return {"insert": name, "documents": list(docs), **more}


def find(name, filter_=None, **more):
    return {"find": name, "filter": filter_ or {}, **more}


def agg(name, *pipeline):
    return {"aggregate": name, "pipeline": list(pipeline), "cursor": {}}


def update(name, q, u, **more):
    return {"update": name, "updates": [{"q": q, "u": u, **more}]}


def delete(name, q, limit=0):
    return {"delete": name, "deletes": [{"q": q, "limit": limit}]}


def index(name, key, ix="ix", **more):
    return {"createIndexes": name, "indexes": [{"key": key, "name": ix, **more}]}


#: mongod keeps a time-series collection's rows in `system.buckets.<name>`
#: and lists that collection, and `system.views`, beside it. Those two are
#: how mongod stores the data, not something a client asked for, so the
#: listings here leave them out; the scenarios that name a buckets
#: collection outright are kept.
LIST = {"listCollections": 1, "filter": {"name": {"$in": ["w", "u", "v"]}}}
NAMES = {
    "listCollections": 1,
    "nameOnly": True,
    "filter": {"name": {"$not": {"$regex": "^system[.]"}}},
}
META = ts(metaField="m")
W = create("w", META)
ROWS = [
    {"_id": 1, "t": at(0), "m": "a", "v": 1},
    {"_id": 2, "t": at(1), "m": "b", "v": 2},
    {"_id": 3, "t": at(2), "m": "a", "v": 3},
    {"_id": 4, "t": at(90), "m": "a", "v": 4},
]
SEED = [W, insert("w", *ROWS)]
ALL = find("w", sort={"_id": 1})
NO_ID = {"_id": 0}

SCENARIOS = [
    ("create: minimal", [create("w", ts()), LIST]),
    ("create: with meta", [W, LIST]),
    ("create: names listed ~", [W, NAMES]),
    ("create: granularity minutes", [create("w", ts(granularity="minutes")), LIST]),
    ("create: granularity hours", [create("w", ts(granularity="hours")), LIST]),
    ("create: granularity bad", [create("w", ts(granularity="days")), LIST]),
    ("create: granularity number", [create("w", ts(granularity=1))]),
    (
        "create: bucket span pair",
        [create("w", ts(bucketMaxSpanSeconds=600, bucketRoundingSeconds=600)), LIST],
    ),
    ("create: bucket span alone", [create("w", ts(bucketMaxSpanSeconds=600))]),
    ("create: bucket rounding alone", [create("w", ts(bucketRoundingSeconds=600))]),
    (
        "create: bucket pair unequal",
        [create("w", ts(bucketMaxSpanSeconds=600, bucketRoundingSeconds=300))],
    ),
    (
        "create: bucket pair with granularity",
        [
            create(
                "w", ts(granularity="minutes", bucketMaxSpanSeconds=600, bucketRoundingSeconds=600)
            )
        ],
    ),
    (
        "create: bucket span zero",
        [create("w", ts(bucketMaxSpanSeconds=0, bucketRoundingSeconds=0))],
    ),
    (
        "create: bucket span huge",
        [create("w", ts(bucketMaxSpanSeconds=31536001, bucketRoundingSeconds=31536001))],
    ),
    (
        "create: bucket span string",
        [create("w", ts(bucketMaxSpanSeconds="600", bucketRoundingSeconds=600))],
    ),
    ("create: no timeField", [create("w", {"metaField": "m"})]),
    ("create: timeField number", [create("w", {"timeField": 5})]),
    ("create: timeField empty", [create("w", {"timeField": ""})]),
    ("create: timeField dotted", [create("w", {"timeField": "a.t"})]),
    ("create: timeField _id", [create("w", {"timeField": "_id"})]),
    ("create: timeField dollar", [create("w", {"timeField": "$t"})]),
    ("create: metaField same", [create("w", ts(metaField="t"))]),
    ("create: metaField _id", [create("w", ts(metaField="_id"))]),
    ("create: metaField dotted", [create("w", ts(metaField="a.m"))]),
    ("create: metaField number", [create("w", ts(metaField=5))]),
    ("create: metaField null", [create("w", ts(metaField=None)), LIST]),
    ("create: unknown field", [create("w", ts(bogus=1))]),
    ("create: empty", [create("w", {})]),
    ("create: not a document", [create("w", "t")]),
    ("create: null", [{"create": "w", "timeseries": None}]),
    ("create: capped too", [create("w", ts(), capped=True, size=4096)]),
    ("create: validator too", [create("w", ts(), validator={"v": {"$gt": 0}}), LIST]),
    ("create: viewOn too", [create("w", ts(), viewOn="x", pipeline=[])]),
    ("create: expireAfterSeconds", [create("w", ts(), expireAfterSeconds=3600), LIST]),
    ("create: expireAfterSeconds bad", [create("w", ts(), expireAfterSeconds=-1)]),
    ("create: expireAfterSeconds string", [create("w", ts(), expireAfterSeconds="60")]),
    (
        "create: clusteredIndex too",
        [create("w", ts(), clusteredIndex={"key": {"_id": 1}, "unique": True})],
    ),
    ("create: collation too", [create("w", ts(), collation={"locale": "en", "strength": 2}), LIST]),
    ("create: twice same", [W, W]),
    ("create: twice other", [W, create("w", ts())]),
    ("create: over a plain collection", [create("w"), W]),
    ("create: plain over time-series", [W, create("w")]),
    ("create: buckets name by hand", [create("system.buckets.w")]),
    ("create: on system.buckets prefix", [create("system.buckets.w", ts())]),
    (
        "create: changeStreamPreAndPostImages",
        [create("w", ts(), changeStreamPreAndPostImages={"enabled": True})],
    ),
    ("create: storageEngine", [create("w", ts(), storageEngine={"wiredTiger": {}}), LIST]),
    (
        "create: timeseriesBucketsMayHaveMixedSchemaData",
        [create("w", ts(), timeseriesBucketsMayHaveMixedSchemaData=True)],
    ),
    ("insert: one", [W, insert("w", ROWS[0]), ALL]),
    ("insert: several", [*SEED, ALL]),
    ("insert: no time field", [W, insert("w", {"_id": 1, "m": "a"}), ALL]),
    ("insert: time null", [W, insert("w", {"_id": 1, "t": None})]),
    ("insert: time string", [W, insert("w", {"_id": 1, "t": "2026-01-01"})]),
    ("insert: time number", [W, insert("w", {"_id": 1, "t": 5})]),
    ("insert: time array", [W, insert("w", {"_id": 1, "t": [at(0)]})]),
    (
        "insert: time before epoch",
        [
            W,
            insert(
                "w", {"_id": 1, "t": datetime.datetime(1960, 1, 1, tzinfo=datetime.timezone.utc)}
            ),
            ALL,
        ],
    ),
    (
        "insert: time far future",
        [
            W,
            insert(
                "w", {"_id": 1, "t": datetime.datetime(2200, 1, 1, tzinfo=datetime.timezone.utc)}
            ),
            ALL,
        ],
    ),
    ("insert: no meta", [W, insert("w", {"_id": 1, "t": at(0)}), ALL]),
    (
        "insert: meta document",
        [W, insert("w", {"_id": 1, "t": at(0), "m": {"k": 1}}), find("w", {"m.k": 1})],
    ),
    (
        "insert: meta array",
        [W, insert("w", {"_id": 1, "t": at(0), "m": [1, 2]}), find("w", {"m": 1})],
    ),
    ("insert: duplicate _id", [W, insert("w", ROWS[0]), insert("w", {**ROWS[0], "v": 9}), ALL]),
    ("insert: duplicate _id one batch", [W, insert("w", ROWS[0], ROWS[0]), {"count": "w"}]),
    (
        "insert: no _id",
        [
            W,
            insert("w", {"t": at(0), "v": 1}),
            find("w", projection={"_id": 0, "id": {"$type": "$_id"}, "v": 1}),
        ],
    ),
    ("insert: mixed good and bad, ordered", [W, insert("w", ROWS[0], {"_id": 9}, ROWS[1]), ALL]),
    (
        "insert: mixed good and bad, unordered",
        [W, insert("w", ROWS[0], {"_id": 9}, ROWS[1], ordered=False), ALL],
    ),
    (
        "insert: field order kept",
        [W, insert("w", {"v": 1, "m": "a", "_id": 1, "t": at(0), "z": 2}), find("w")],
    ),
    ("insert: into buckets by hand", [W, insert("system.buckets.w", {"_id": 1})]),
    ("insert: creates nothing implicit", [insert("w", ROWS[0]), LIST]),
    ("insert: _id array", [W, insert("w", {"_id": [1], "t": at(0)})]),
    ("insert: dollar field", [W, insert("w", {"_id": 1, "t": at(0), "$x": 1})]),
    ("read: natural order", [*SEED, find("w", projection={"_id": 1})]),
    (
        "read: filter on meta",
        [*SEED, find("w", {"m": "a"}, sort={"_id": 1}, projection={"_id": 1})],
    ),
    (
        "read: filter on time",
        [
            *SEED,
            find(
                "w", {"t": {"$gte": at(1), "$lt": at(60)}}, sort={"_id": 1}, projection={"_id": 1}
            ),
        ],
    ),
    (
        "read: filter on value",
        [*SEED, find("w", {"v": {"$gt": 2}}, sort={"_id": 1}, projection={"_id": 1})],
    ),
    ("read: filter on _id", [*SEED, find("w", {"_id": 2})]),
    ("read: sort by time desc", [*SEED, find("w", sort={"t": -1}, projection={"_id": 1})]),
    (
        "read: sort by meta then time",
        [*SEED, find("w", sort={"m": 1, "t": -1}, projection={"_id": 1})],
    ),
    (
        "read: limit and skip",
        [*SEED, find("w", sort={"t": 1}, skip=1, limit=2, projection={"_id": 1})],
    ),
    ("read: projection excludes time", [*SEED, find("w", {"_id": 1}, projection={"t": 0})]),
    ("read: count", [*SEED, {"count": "w"}, {"count": "w", "query": {"m": "a"}}]),
    ("read: distinct ~", [*SEED, {"distinct": "w", "key": "m"}]),
    (
        "read: $group",
        [*SEED, agg("w", {"$group": {"_id": "$m", "n": {"$sum": "$v"}}}, {"$sort": {"_id": 1}})],
    ),
    (
        "read: $match then $sort",
        [*SEED, agg("w", {"$match": {"m": "a"}}, {"$sort": {"t": -1}}, {"$project": {"_id": 1}})],
    ),
    (
        "read: $dateTrunc bucket",
        [
            *SEED,
            agg(
                "w",
                {
                    "$group": {
                        "_id": {"$dateTrunc": {"date": "$t", "unit": "hour"}},
                        "n": {"$sum": 1},
                    }
                },
                {"$sort": {"_id": 1}},
            ),
        ],
    ),
    (
        "read: $setWindowFields",
        [
            *SEED,
            agg(
                "w",
                {
                    "$setWindowFields": {
                        "partitionBy": "$m",
                        "sortBy": {"t": 1},
                        "output": {
                            "s": {"$sum": "$v", "window": {"documents": ["unbounded", "current"]}}
                        },
                    }
                },
                {"$project": {"s": 1}},
                {"$sort": {"_id": 1}},
            ),
        ],
    ),
    (
        "read: $lookup from",
        [
            *SEED,
            insert("u", {"_id": 1, "k": "a"}),
            agg(
                "u",
                {"$lookup": {"from": "w", "localField": "k", "foreignField": "m", "as": "r"}},
                {"$project": {"n": {"$size": "$r"}}},
            ),
        ],
    ),
    (
        "read: $unionWith",
        [
            *SEED,
            insert("u", {"_id": 9}),
            agg("u", {"$unionWith": "w"}, {"$project": {"_id": 1}}, {"$sort": {"_id": 1}}),
        ],
    ),
    (
        "read: $collStats",
        [*SEED, agg("w", {"$collStats": {"count": {}}}, {"$project": {"count": 1}})],
    ),
    (
        "read: $indexStats",
        [*SEED, agg("w", {"$indexStats": {}}, {"$project": {"name": 1}}, {"$sort": {"name": 1}})],
    ),
    ("read: explain", [*SEED, {"explain": find("w", {"m": "a"})}]),
    ("read: buckets collection", [*SEED, {"count": "system.buckets.w"}]),
    (
        "read: buckets shape",
        [
            *SEED,
            agg(
                "system.buckets.w",
                {
                    "$project": {
                        "_id": 0,
                        "meta": 1,
                        "keys": {"$map": {"input": {"$objectToArray": "$$ROOT"}, "in": "$$this.k"}},
                    }
                },
                {"$sort": {"meta": 1}},
                {"$limit": 1},
            ),
        ],
    ),
    ("read: hint _id", [*SEED, find("w", hint={"_id": 1})]),
    ("read: hint $natural", [*SEED, find("w", hint={"$natural": 1}, projection={"_id": 1})]),
    ("read: tailable", [*SEED, find("w", tailable=True)]),
    ("read: change stream", [*SEED, agg("w", {"$changeStream": {}})]),
    ("read: min and max", [*SEED, find("w", min={"t": at(1)}, hint={"t": 1})]),
    (
        "write: update multi on meta",
        [
            *SEED,
            update("w", {"m": "a"}, {"$set": {"m": "z"}}, multi=True),
            find("w", sort={"_id": 1}, projection={"m": 1}),
        ],
    ),
    (
        "write: update multi on value",
        [
            *SEED,
            update("w", {"m": "a"}, {"$set": {"v": 0}}, multi=True),
            find("w", sort={"_id": 1}, projection={"v": 1}),
        ],
    ),
    (
        "write: update multi by value",
        [
            *SEED,
            update("w", {"v": {"$gt": 2}}, {"$inc": {"v": 10}}, multi=True),
            find("w", sort={"_id": 1}, projection={"v": 1}),
        ],
    ),
    (
        "write: update one",
        [
            *SEED,
            update("w", {"m": "a"}, {"$set": {"v": 0}}),
            find("w", sort={"_id": 1}, projection={"v": 1}),
        ],
    ),
    (
        "write: update one by _id",
        [*SEED, update("w", {"_id": 1}, {"$set": {"v": 0}}), find("w", {"_id": 1})],
    ),
    (
        "write: update time field",
        [*SEED, update("w", {"_id": 1}, {"$set": {"t": at(5)}}, multi=True), find("w", {"_id": 1})],
    ),
    (
        "write: update unset time",
        [*SEED, update("w", {"_id": 1}, {"$unset": {"t": 1}}, multi=True), find("w", {"_id": 1})],
    ),
    (
        "write: update time to string",
        [*SEED, update("w", {"_id": 1}, {"$set": {"t": "x"}}, multi=True)],
    ),
    ("write: update _id", [*SEED, update("w", {"_id": 1}, {"$set": {"_id": 9}}, multi=True)]),
    (
        "write: replacement",
        [*SEED, update("w", {"_id": 1}, {"t": at(0), "v": 9}), find("w", {"_id": 1})],
    ),
    ("write: replacement without time", [*SEED, update("w", {"_id": 1}, {"v": 9})]),
    (
        "write: pipeline update",
        [
            *SEED,
            update("w", {"m": "a"}, [{"$set": {"v": {"$add": ["$v", 1]}}}], multi=True),
            find("w", sort={"_id": 1}, projection={"v": 1}),
        ],
    ),
    ("write: upsert", [*SEED, update("w", {"m": "q"}, {"$set": {"v": 1}}, upsert=True)]),
    (
        "write: upsert with time",
        [
            *SEED,
            update("w", {"m": "q", "t": at(9)}, {"$set": {"v": 1}}, upsert=True, multi=True),
            find("w", {"m": "q"}, projection=NO_ID),
        ],
    ),
    (
        "write: arrayFilters",
        [
            *SEED,
            update("w", {"_id": 1}, {"$set": {"a.$[e]": 1}}, multi=True, arrayFilters=[{"e": 1}]),
        ],
    ),
    (
        "write: update with hint",
        [*SEED, update("w", {"m": "a"}, {"$set": {"v": 0}}, multi=True, hint={"m": 1, "t": 1})],
    ),
    ("write: delete many on meta", [*SEED, delete("w", {"m": "a"}), ALL]),
    ("write: delete many by value", [*SEED, delete("w", {"v": {"$gt": 2}}), ALL]),
    ("write: delete one", [*SEED, delete("w", {"m": "a"}, limit=1), {"count": "w"}]),
    ("write: delete all", [*SEED, delete("w", {}), {"count": "w"}]),
    ("write: delete by time", [*SEED, delete("w", {"t": {"$lt": at(2)}}), ALL]),
    (
        "write: findAndModify update",
        [
            *SEED,
            {"findAndModify": "w", "query": {"_id": 1}, "update": {"$set": {"v": 0}}, "new": True},
        ],
    ),
    (
        "write: findAndModify remove",
        [
            *SEED,
            {"findAndModify": "w", "query": {"m": "a"}, "sort": {"t": -1}, "remove": True},
            ALL,
        ],
    ),
    (
        "write: findAndModify upsert",
        [
            *SEED,
            {
                "findAndModify": "w",
                "query": {"m": "q"},
                "update": {"$set": {"v": 0}},
                "upsert": True,
            },
        ],
    ),
    (
        "write: findAndModify replacement",
        [
            *SEED,
            {
                "findAndModify": "w",
                "query": {"_id": 1},
                "update": {"t": at(0), "v": 7},
                "new": True,
            },
        ],
    ),
    (
        "write: bulkWrite",
        [
            *SEED,
            {
                "admin": {
                    "bulkWrite": 1,
                    "ops": [
                        {"insert": 0, "document": {"_id": 7, "t": at(3), "m": "a"}},
                        {
                            "update": 0,
                            "filter": {"m": "b"},
                            "updateMods": {"$set": {"v": 0}},
                            "multi": True,
                        },
                        {"delete": 0, "filter": {"_id": 1}, "multi": True},
                    ],
                    "nsInfo": [{"ns": f"{DB}.w"}],
                }
            },
            ALL,
        ],
    ),
    (
        "write: $out into a new time-series",
        [
            *SEED,
            agg("w", {"$match": {"m": "a"}}, {"$out": {"db": DB, "coll": "u", "timeseries": META}}),
            LIST,
            find("u", sort={"_id": 1}, projection={"_id": 1}),
        ],
    ),
    (
        "write: $out from plain",
        [
            insert("u", *ROWS),
            agg("u", {"$out": {"db": DB, "coll": "w", "timeseries": META}}),
            LIST,
            {"count": "w"},
        ],
    ),
    (
        "write: $out over an existing time-series",
        [*SEED, insert("u", {"_id": 9, "t": at(7), "m": "x"}), agg("u", {"$out": "w"}), LIST, ALL],
    ),
    (
        "write: $out with other options",
        [
            *SEED,
            insert("u", {"_id": 9, "t": at(7)}),
            agg("u", {"$out": {"db": DB, "coll": "w", "timeseries": ts()}}),
        ],
    ),
    ("write: $out time-series to plain", [*SEED, agg("w", {"$out": "u"}), LIST, {"count": "u"}]),
    (
        "write: $merge into",
        [*SEED, insert("u", {"_id": 9, "t": at(7)}), agg("u", {"$merge": {"into": "w"}})],
    ),
    ("write: $merge from", [*SEED, agg("w", {"$merge": {"into": "u"}}), {"count": "u"}]),
    ("index: default list", [W, {"listIndexes": "w"}]),
    ("index: default without meta", [create("w", ts()), {"listIndexes": "w"}]),
    ("index: on meta ~", [W, index("w", {"m": 1}), {"listIndexes": "w"}]),
    ("index: on time ~", [W, index("w", {"t": -1}), {"listIndexes": "w"}]),
    ("index: on a measurement ~", [W, index("w", {"v": 1}), {"listIndexes": "w"}]),
    (
        "index: on meta subfield and time ~",
        [W, index("w", {"m.k": 1, "t": 1}), {"listIndexes": "w"}],
    ),
    ("index: unique", [W, index("w", {"m": 1}, unique=True)]),
    ("index: sparse", [W, index("w", {"m": 1}, sparse=True)]),
    (
        "index: partial",
        [W, index("w", {"m": 1}, partialFilterExpression={"m": "a"}), {"listIndexes": "w"}],
    ),
    (
        "index: partial on a measurement",
        [W, index("w", {"m": 1}, partialFilterExpression={"v": {"$gt": 1}}), {"listIndexes": "w"}],
    ),
    ("index: ttl", [W, index("w", {"t": 1}, expireAfterSeconds=3600)]),
    (
        "index: ttl with partial on meta",
        [
            W,
            index("w", {"t": 1}, expireAfterSeconds=3600, partialFilterExpression={"m": "a"}),
            {"listIndexes": "w"},
        ],
    ),
    ("index: hidden ~", [W, index("w", {"m": 1}, hidden=True), {"listIndexes": "w"}]),
    ("index: 2dsphere ~", [W, index("w", {"loc": "2dsphere"}), {"listIndexes": "w"}]),
    ("index: text", [W, index("w", {"v": "text"})]),
    ("index: hashed", [W, index("w", {"m": "hashed"})]),
    ("index: wildcard", [W, index("w", {"$**": 1})]),
    ("index: on _id", [W, index("w", {"_id": 1})]),
    (
        "index: same as default ~",
        [W, index("w", {"m": 1, "t": 1}, ix="again"), {"listIndexes": "w"}],
    ),
    ("index: drop default", [W, {"dropIndexes": "w", "index": "m_1_t_1"}, {"listIndexes": "w"}]),
    (
        "index: drop all",
        [W, index("w", {"v": 1}), {"dropIndexes": "w", "index": "*"}, {"listIndexes": "w"}],
    ),
    (
        "index: drop by key",
        [W, index("w", {"v": 1}), {"dropIndexes": "w", "index": {"v": 1}}, {"listIndexes": "w"}],
    ),
    (
        "index: on buckets by hand",
        [W, index("system.buckets.w", {"meta": 1}), {"listIndexes": "w"}],
    ),
    ("index: list buckets", [W, {"listIndexes": "system.buckets.w"}]),
    ("index: implicit create", [index("w", {"m": 1}), LIST]),
    (
        "collMod: granularity up",
        [W, {"collMod": "w", "timeseries": {"granularity": "minutes"}}, LIST],
    ),
    (
        "collMod: granularity down",
        [
            create("w", ts(granularity="hours")),
            {"collMod": "w", "timeseries": {"granularity": "minutes"}},
        ],
    ),
    (
        "collMod: granularity same",
        [W, {"collMod": "w", "timeseries": {"granularity": "seconds"}}, LIST],
    ),
    ("collMod: granularity bad", [W, {"collMod": "w", "timeseries": {"granularity": "days"}}]),
    (
        "collMod: bucket pair up",
        [
            W,
            {
                "collMod": "w",
                "timeseries": {"bucketMaxSpanSeconds": 7200, "bucketRoundingSeconds": 7200},
            },
            LIST,
        ],
    ),
    (
        "collMod: bucket pair down",
        [
            W,
            {
                "collMod": "w",
                "timeseries": {"bucketMaxSpanSeconds": 60, "bucketRoundingSeconds": 60},
            },
        ],
    ),
    ("collMod: timeField", [W, {"collMod": "w", "timeseries": {"timeField": "x"}}]),
    ("collMod: metaField", [W, {"collMod": "w", "timeseries": {"metaField": "x"}}]),
    ("collMod: empty timeseries", [W, {"collMod": "w", "timeseries": {}}]),
    ("collMod: expireAfterSeconds", [W, {"collMod": "w", "expireAfterSeconds": 60}, LIST]),
    (
        "collMod: expireAfterSeconds off",
        [
            create("w", ts(), expireAfterSeconds=60),
            {"collMod": "w", "expireAfterSeconds": "off"},
            LIST,
        ],
    ),
    ("collMod: expireAfterSeconds bad", [W, {"collMod": "w", "expireAfterSeconds": "on"}]),
    ("collMod: validator", [W, {"collMod": "w", "validator": {"v": 1}}]),
    (
        "collMod: index hidden",
        [W, {"collMod": "w", "index": {"name": "m_1_t_1", "hidden": True}}, {"listIndexes": "w"}],
    ),
    (
        "collMod: timeseries on plain",
        [create("u"), {"collMod": "u", "timeseries": {"granularity": "hours"}}],
    ),
    (
        "collMod: mixed schema flag",
        [W, {"collMod": "w", "timeseriesBucketsMayHaveMixedSchemaData": True}],
    ),
    ("admin: drop", [*SEED, {"drop": "w"}, NAMES]),
    ("admin: drop buckets by hand", [*SEED, {"drop": "system.buckets.w"}, NAMES]),
    ("admin: rename", [*SEED, {"admin": {"renameCollection": f"{DB}.w", "to": f"{DB}.u"}}, NAMES]),
    (
        "admin: rename onto",
        [
            *SEED,
            create("u"),
            {"admin": {"renameCollection": f"{DB}.u", "to": f"{DB}.w", "dropTarget": True}},
        ],
    ),
    (
        "admin: rename buckets",
        [
            *SEED,
            {
                "admin": {
                    "renameCollection": f"{DB}.system.buckets.w",
                    "to": f"{DB}.system.buckets.u",
                }
            },
        ],
    ),
    ("admin: convertToCapped", [*SEED, {"convertToCapped": "w", "size": 4096}]),
    (
        "admin: view on it",
        [
            *SEED,
            create("v", viewOn="w", pipeline=[{"$match": {"m": "a"}}]),
            find("v", sort={"_id": 1}, projection={"_id": 1}),
        ],
    ),
    (
        "admin: view on buckets",
        [*SEED, create("v", viewOn="system.buckets.w", pipeline=[]), {"count": "v"}],
    ),
    ("admin: collStats", [*SEED, {"collStats": "w"}]),
    ("admin: validate", [*SEED, {"validate": "w"}]),
    ("admin: dropDatabase", [*SEED, {"dropDatabase": 1}, NAMES]),
    (
        "admin: listCollections type filter",
        [
            *SEED,
            create("u"),
            {"listCollections": 1, "filter": {"type": "timeseries"}, "nameOnly": True},
        ],
    ),
    (
        "admin: listCollections authorizedCollections",
        [W, {"listCollections": 1, "nameOnly": True, "authorizedCollections": True}],
    ),
    ("admin: compact", [*SEED, {"compact": "w"}]),
    ("admin: reIndex", [*SEED, {"reIndex": "w"}]),
    (
        "admin: cloneCollectionAsCapped",
        [*SEED, {"cloneCollectionAsCapped": "w", "toCollection": "u", "size": 4096}],
    ),
    (
        "ttl: collection level kept in options",
        [
            create("w", META, expireAfterSeconds=3600),
            insert("w", {"_id": 1, "t": at(0), "m": "a"}),
            LIST,
            {"listIndexes": "w"},
        ],
    ),
]


def typed(v):
    if isinstance(v, bool) or v is None or isinstance(v, str):
        return v
    if isinstance(v, Int64):
        return f"long:{int(v)}"
    if isinstance(v, int):
        return f"int:{v}"
    if isinstance(v, float):
        return f"double:{v}"
    if isinstance(v, dict):
        return {k: typed(x) for k, x in v.items() if k not in NOISE}
    if isinstance(v, (list, tuple)):
        return [typed(x) for x in v]
    if isinstance(v, ObjectId):
        return "ObjectId(...)"
    return repr(v)


#: A wrong-type error lists the types it would take in an order that differs
#: between patch releases of mongod, so the list is compared as a set.
TYPE_LIST = re.compile(r"expected types '\[([^\]]*)\]'")
UUID = re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
#: A collation sort key, as a duplicate-key error quotes it. mongod's ICU and
#: ICU4X order strings alike and number the weights differently, so the bytes
#: are not compared -- only that a key is reported.
SORT_KEY = re.compile(r"CollationKey\(0x[0-9a-f]+\)")


def scrub(text):
    return SORT_KEY.sub("CollationKey(...)", UUID.sub("<uuid>", text))


def sorted_types(match):
    return "expected types '[" + ", ".join(sorted(match.group(1).split(", "))) + "]'"


def outcome(client, command, bag):
    database = DB
    if set(command) == {"admin"}:
        database, command = "admin", command["admin"]
    try:
        reply = dict(client[database].command(command))
    except PyMongoError as exc:
        details = getattr(exc, "details", None) or {}
        message = details.get("errmsg") or f"CLIENT {exc!r}"
        return ("ERR", details.get("code"), scrub(TYPE_LIST.sub(sorted_types, message))[:400])
    cursor = reply.get("cursor")
    if isinstance(cursor, dict) and "firstBatch" in cursor:
        if cursor.get("id") and "$changeStream" not in str(command):
            client[database].command(
                {"killCursors": command[next(iter(command))], "cursors": [cursor["id"]]}
            )
            return ("OK", "CURSOR LEFT OPEN")
        batch = [typed(d) for d in cursor["firstBatch"]]
        # `listIndexes` order is a difference of its own, recorded with the
        # index probe (mongod: creation order), so it is not counted again.
        if "listIndexes" in command:
            batch.sort(key=repr)
        if "explain" in command or "$changeStream" in str(command):
            return ("OK", "cursor")
        return ("OK", sorted(batch, key=repr) if bag else batch)
    if "explain" in command:
        return ("OK", "explained")
    reply = typed(reply)
    for error in reply.get("writeErrors") or []:
        error["errmsg"] = scrub(error.get("errmsg", ""))
        if error.get("hexEncoded"):
            error["keyValue"] = {k: "<sort key>" for k in error.get("keyValue", {})}
    if bag and "values" in reply:
        reply["values"] = sorted(reply["values"], key=repr)
    return ("OK", reply)


def run(client):
    results = {}
    for label, commands in SCENARIOS:
        client.drop_database(DB)
        bag = label.endswith("~")
        results[label] = [outcome(client, command, bag) for command in commands]
    return results


def self_check(expected):
    """The probe is worthless unless mongod itself stores and returns a row."""
    got = expected["insert: one"]
    if got[0][0] != "OK" or got[2][0] != "OK" or len(got[2][1]) != 1:
        sys.exit(f"SELF-CHECK FAILED on the reference server: {got}")


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else ""
    with probe_targets(replica_set="secantus") as (mon, targets):
        divergent = {label: 0 for label, _ in targets}
        expected = run(mon)
        self_check(expected)
        got = {name: run(cli) for name, cli in targets}
        for label, _ in SCENARIOS:
            off = {name for name, g in got.items() if g[label] != expected[label]}
            if only and not label.startswith(only):
                continue
            if not off:
                continue
            for name in off:
                divergent[name] += 1
            print(f"  {label}")
            for i, want in enumerate(expected[label]):
                differs = [name for name in off if got[name][label][i] != want]
                if not differs:
                    continue
                print(f"    [{i}] mongod  : {want}")
                for name in differs:
                    print(f"    [{i}] {name:8s}: {got[name][label][i]}")
        return report("time_series", len(SCENARIOS), divergent)


if __name__ == "__main__":
    sys.exit(main())
