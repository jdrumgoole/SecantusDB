"""Differential-probe collation against a real mongod.

Four groups of scenarios, each compared reply for reply:

* READS -- one seeded collection of strings that differ by case, accent,
  punctuation and digit runs, read under each collation by `find` (sort,
  equality, range, `$in`, `$ne`), `distinct`, `count` and `aggregate`.
* SPECS -- what each command answers to a collation document that is
  malformed, incomplete or names an unknown locale.
* EXPRESSIONS -- the aggregation operators and query operators that do, and
  do not, compare strings by the collation.
* STATEFUL -- a fresh database per scenario: a collection's default collation
  and what inherits it, collated indexes (build, list, unique, hint, drop),
  views, and the write commands.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/collation.py

A label ending in `~` compares a cursor's documents as an unordered bag: the
order of `$group` output and of `distinct` values is not defined.

`collation_order.py` is the older, narrower probe of sort order alone.
"""

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
DB = "collation_probe"

STRINGS = [
    "a",
    "A",
    "á",
    "Á",
    "b",
    "B",
    "ab",
    "a b",
    "a-b",
    "10",
    "9",
    "2",
    "a10",
    "a9",
    "",
    None,
    5,
    ["a", "B"],
    {"k": "A"},
    "côte",
    "coté",
    "cote",
    "côté",
    "ss",
    "ae",
    "z",
    "Z",
    "á",  # a + combining acute: canonically equal to á
    "_a",
    " a",
]


def seed(db):
    db.c.insert_many([{"_id": i, "s": v, "n": i % 3} for i, v in enumerate(STRINGS)])
    db.c.insert_one({"_id": 100, "n": 1})
    db.o.insert_many(
        [{"_id": 0, "k": "A"}, {"_id": 1, "k": "á"}, {"_id": 2, "k": "B"}, {"_id": 3, "k": "zz"}]
    )


EN = {"locale": "en"}
S1, S2 = {"locale": "en", "strength": 1}, {"locale": "en", "strength": 2}

COLLATIONS = [
    ("none", None),
    ("simple", {"locale": "simple"}),
    ("en", EN),
    ("s1", S1),
    ("s2", S2),
    ("s3", {"locale": "en", "strength": 3}),
    ("s4", {"locale": "en", "strength": 4}),
    ("s5", {"locale": "en", "strength": 5}),
    ("s1 caseLevel", {"locale": "en", "strength": 1, "caseLevel": True}),
    ("s2 caseLevel", {"locale": "en", "strength": 2, "caseLevel": True}),
    ("s3 caseLevel", {"locale": "en", "strength": 3, "caseLevel": True}),
    ("upper first", {"locale": "en", "caseFirst": "upper"}),
    ("lower first", {"locale": "en", "caseFirst": "lower"}),
    ("s1 upper first", {"locale": "en", "strength": 1, "caseFirst": "upper"}),
    ("numeric", {"locale": "en", "numericOrdering": True}),
    ("s2 numeric", {"locale": "en", "strength": 2, "numericOrdering": True}),
    ("shifted", {"locale": "en", "alternate": "shifted"}),
    ("s1 shifted", {"locale": "en", "strength": 1, "alternate": "shifted"}),
    ("s4 shifted", {"locale": "en", "strength": 4, "alternate": "shifted"}),
    ("shifted space", {"locale": "en", "alternate": "shifted", "maxVariable": "space"}),
    ("backwards", {"locale": "en", "backwards": True}),
    ("s2 backwards", {"locale": "en", "strength": 2, "backwards": True}),
    ("normalization", {"locale": "en", "normalization": True}),
    ("en_US", {"locale": "en_US"}),
    ("root", {"locale": "root"}),
    ("fr", {"locale": "fr"}),
    ("fr_CA", {"locale": "fr_CA"}),
    ("de", {"locale": "de"}),
    ("sv", {"locale": "sv"}),
]

ONLY_ID = {"_id": 1}


def with_collation(command, collation):
    return command if collation is None else {**command, "collation": collation}


def reads():
    out = []
    for name, c in COLLATIONS:

        def add(label, command, c=c, name=name):
            out.append((f"{name}: {label}", with_collation(command, c)))

        add("sort", {"find": "c", "sort": {"s": 1, "_id": 1}, "projection": ONLY_ID})
        add("sort desc", {"find": "c", "sort": {"s": -1, "_id": 1}, "projection": ONLY_ID})
        add("eq a", {"find": "c", "filter": {"s": "a"}, "sort": ONLY_ID, "projection": ONLY_ID})
        add("eq cote", {"find": "c", "filter": {"s": "cote"}, "sort": ONLY_ID})
        add("eq a b", {"find": "c", "filter": {"s": "ab"}, "sort": ONLY_ID})
        add("gt a", {"find": "c", "filter": {"s": {"$gt": "a"}}, "sort": ONLY_ID})
        add("lte B", {"find": "c", "filter": {"s": {"$lte": "B"}}, "sort": ONLY_ID})
        add("in", {"find": "c", "filter": {"s": {"$in": ["A", "9"]}}, "sort": ONLY_ID})
        add("ne", {"find": "c", "filter": {"s": {"$ne": "a"}}, "sort": ONLY_ID})
        add(
            "range digits",
            {"find": "c", "filter": {"s": {"$gt": "2", "$lt": "a"}}, "sort": ONLY_ID},
        )
        add("count", {"count": "c", "query": {"s": "a"}})
        add("distinct ~", {"distinct": "c", "key": "s", "query": {"n": 0}})
        add(
            "group ~",
            {
                "aggregate": "c",
                "pipeline": [{"$group": {"_id": "$s", "n": {"$sum": 1}}}, {"$project": {"n": 1}}],
                "cursor": {},
            },
        )
        add(
            "$sort stage",
            {
                "aggregate": "c",
                "pipeline": [{"$sort": {"s": 1, "_id": 1}}, {"$project": ONLY_ID}],
                "cursor": {},
            },
        )
    return out


BAD_SPECS = [
    ("empty", {}),
    ("no locale", {"strength": 2}),
    ("locale number", {"locale": 5}),
    ("locale empty", {"locale": ""}),
    ("locale unknown", {"locale": "zz_nope"}),
    ("locale upper", {"locale": "EN"}),
    ("locale hyphen", {"locale": "en-US"}),
    ("locale posix", {"locale": "en_US_POSIX"}),
    ("locale keyword", {"locale": "de@collation=phonebook"}),
    ("locale bad keyword", {"locale": "de@collation=nope"}),
    ("simple with strength", {"locale": "simple", "strength": 1}),
    ("simple with caseLevel false", {"locale": "simple", "caseLevel": False}),
    ("strength 0", {"locale": "en", "strength": 0}),
    ("strength 6", {"locale": "en", "strength": 6}),
    ("strength 1.5", {"locale": "en", "strength": 1.5}),
    ("strength 2.0", {"locale": "en", "strength": 2.0}),
    ("strength long", {"locale": "en", "strength": Int64(2)}),
    ("strength string", {"locale": "en", "strength": "2"}),
    ("strength null", {"locale": "en", "strength": None}),
    ("caseLevel number", {"locale": "en", "caseLevel": 1}),
    ("caseFirst bad", {"locale": "en", "caseFirst": "x"}),
    ("caseFirst off", {"locale": "en", "caseFirst": "off"}),
    ("caseFirst number", {"locale": "en", "caseFirst": 1}),
    ("alternate bad", {"locale": "en", "alternate": "x"}),
    ("alternate non-ignorable", {"locale": "en", "alternate": "non-ignorable"}),
    ("maxVariable bad", {"locale": "en", "maxVariable": "x"}),
    ("maxVariable punct", {"locale": "en", "maxVariable": "punct"}),
    ("numericOrdering string", {"locale": "en", "numericOrdering": "true"}),
    ("backwards number", {"locale": "en", "backwards": 1}),
    ("backwards with strength 1", {"locale": "en", "strength": 1, "backwards": True}),
    ("normalization number", {"locale": "en", "normalization": 1}),
    ("version right", {"locale": "en", "version": "57.1"}),
    ("version wrong", {"locale": "en", "version": "1"}),
    ("version number", {"locale": "en", "version": 57.1}),
    ("unknown field", {"locale": "en", "bogus": 1}),
    ("not a document", 5),
    ("a string", "en"),
    ("an array", [EN]),
    ("null", None),
]

#: The specs every command but `find` and `createIndexes` is shown.
FEW = ("empty", "locale unknown", "strength 6", "unknown field", "a string", "simple with strength")


def specs():
    out = []
    for name, spec in BAD_SPECS:
        out.append((f"find, {name}", {"find": "c", "filter": {"s": "a"}, "collation": spec}))
        out.append(
            (
                f"createIndexes, {name}",
                [
                    {
                        "createIndexes": "c",
                        "indexes": [{"key": {"s": 1}, "name": "ix", "collation": spec}],
                    },
                    {"listIndexes": "c"},
                    {"dropIndexes": "c", "index": "ix"},
                ],
            )
        )
        if name not in FEW:
            continue
        for label, command in [
            ("aggregate", {"aggregate": "c", "pipeline": [], "cursor": {}}),
            ("count", {"count": "c", "query": {"s": "a"}}),
            ("distinct ~", {"distinct": "c", "key": "s"}),
            ("findAndModify", {"findAndModify": "c", "query": {"s": "nope"}, "remove": True}),
            ("create", {"create": "fresh"}),
            ("create view", {"create": "vw", "viewOn": "c", "pipeline": []}),
            ("explain", {"explain": {"find": "c", "filter": {}, "collation": spec}}),
        ]:
            if label != "explain":
                command = {**command, "collation": spec}
            bag = " ~" if label.endswith("~") else ""
            label = label.removesuffix(" ~")
            out.append((f"{label}, {name}{bag}", [command, {"drop": "fresh"}, {"drop": "vw"}]))
        out.append(
            (
                f"update, {name}",
                {
                    "update": "c",
                    "updates": [{"q": {"s": "nope"}, "u": {"$set": {"x": 1}}, "collation": spec}],
                },
            )
        )
        out.append(
            (
                f"delete, {name}",
                {"delete": "c", "deletes": [{"q": {"s": "nope"}, "limit": 0, "collation": spec}]},
            )
        )
    return out


def agg(*pipeline, coll="c", collation=S2):
    return with_collation({"aggregate": coll, "pipeline": list(pipeline), "cursor": {}}, collation)


def expr(expression, collation=S2):
    return agg({"$limit": 1}, {"$project": {"_id": 0, "r": expression}}, collation=collation)


def match(filter_, collation=S2):
    return with_collation(
        {"find": "c", "filter": filter_, "sort": ONLY_ID, "projection": ONLY_ID}, collation
    )


LOOKUP = {"from": "o", "localField": "s", "foreignField": "k", "as": "m"}
LOOKUP_PIPE = {
    "from": "o",
    "let": {"s": "$s"},
    "pipeline": [{"$match": {"$expr": {"$eq": ["$k", "$$s"]}}}],
    "as": "m",
}
JOINED = [{"$match": {"m": {"$ne": []}}}, {"$project": {"m._id": 1}}, {"$sort": ONLY_ID}]
ARR = {"$literal": ["b", "A", "a", "B", "á"]}

EXPRESSIONS = [
    ("$eq", expr({"$eq": ["a", "A"]})),
    ("$eq s1 accents", expr({"$eq": ["a", "á"]}, S1)),
    ("$ne", expr({"$ne": ["a", "A"]})),
    ("$cmp", expr({"$cmp": ["a", "B"]})),
    ("$cmp no collation", expr({"$cmp": ["a", "B"]}, None)),
    ("$gt", expr({"$gt": ["b", "A"]})),
    ("$lte", expr({"$lte": ["A", "a"]})),
    ("$in", expr({"$in": ["A", ["a", "b"]]})),
    ("$indexOfArray", expr({"$indexOfArray": [["b", "a"], "A"]})),
    ("$setUnion ~", expr({"$setUnion": [["a", "b"], ["A", "c"]]})),
    ("$setIntersection ~", expr({"$setIntersection": [["a", "b"], ["A", "c"]]})),
    ("$setDifference ~", expr({"$setDifference": [["a", "b"], ["A", "c"]]})),
    ("$setEquals", expr({"$setEquals": [["a", "b"], ["A", "B"]]})),
    ("$setIsSubset", expr({"$setIsSubset": [["A"], ["a", "b"]]})),
    ("$max", expr({"$max": ["a", "B"]})),
    ("$min", expr({"$min": ["B", "a"]})),
    ("$max ties", expr({"$max": ["a", "A"]})),
    ("$sortArray", expr({"$sortArray": {"input": ARR, "sortBy": 1}})),
    ("$sortArray no collation", expr({"$sortArray": {"input": ARR, "sortBy": 1}}, None)),
    ("$maxN", expr({"$maxN": {"input": ARR, "n": 2}})),
    ("$minN", expr({"$minN": {"input": ARR, "n": 2}})),
    (
        "$switch",
        expr({"$switch": {"branches": [{"case": {"$eq": ["a", "A"]}, "then": 1}], "default": 0}}),
    ),
    ("$cond", expr({"$cond": [{"$eq": ["a", "A"]}, "same", "different"]})),
    ("$strcasecmp", expr({"$strcasecmp": ["a", "á"]}, S1)),
    ("$regexMatch", expr({"$regexMatch": {"input": "A", "regex": "a"}})),
    ("$indexOfCP", expr({"$indexOfCP": ["xAx", "a"]})),
    ("$toLower", expr({"$toLower": "Á"}, S1)),
    ("$replaceAll", expr({"$replaceAll": {"input": "aAa", "find": "a", "replacement": "x"}})),
    ("$split", expr({"$split": ["aAa", "a"]})),
    ("$eq documents", expr({"$eq": [{"k": "a"}, {"k": "A"}]})),
    ("$eq document keys", expr({"$eq": [{"k": "a"}, {"K": "a"}]})),
    ("$eq arrays", expr({"$eq": [["a"], ["A"]]})),
    ("$eq numeric", expr({"$eq": ["10", "010"]}, {"locale": "en", "numericOrdering": True})),
    ("$cmp numeric", expr({"$cmp": ["10", "9"]}, {"locale": "en", "numericOrdering": True})),
    ("$filter", expr({"$filter": {"input": ARR, "cond": {"$eq": ["$$this", "a"]}}})),
    (
        "$reduce $max",
        expr(
            {"$reduce": {"input": ARR, "initialValue": "", "in": {"$max": ["$$value", "$$this"]}}}
        ),
    ),
    ("match $expr", match({"$expr": {"$eq": ["$s", "A"]}})),
    ("match $expr $in", match({"$expr": {"$in": ["$s", ["A", "B"]]}})),
    ("match $nin", match({"s": {"$nin": ["a", "b"]}})),
    ("match $all", match({"s": {"$all": ["A", "b"]}})),
    ("match $elemMatch", match({"s": {"$elemMatch": {"$eq": "b"}}})),
    ("match $elemMatch range", match({"s": {"$elemMatch": {"$gt": "A", "$lt": "c"}}})),
    ("match $regex", match({"s": {"$regex": "^a$"}})),
    ("match regex literal in $in", match({"s": {"$in": ["B"]}})),
    ("match $not", match({"s": {"$not": {"$eq": "A"}}})),
    ("match $or", match({"$or": [{"s": "B"}, {"s": "Z"}]})),
    ("match $nor", match({"$nor": [{"s": "a"}, {"s": {"$gt": "a"}}]})),
    ("match document", match({"s": {"k": "a"}})),
    ("match dotted", match({"s.k": "a"})),
    ("match array", match({"s": ["A", "b"]})),
    ("match $type", match({"s": {"$type": "string"}, "n": 0})),
    ("match $exists", match({"s": {"$exists": False}})),
    ("match $size", match({"s": {"$size": 2}})),
    ("match $gte null", match({"s": {"$gte": None}})),
    ("match _id", with_collation({"find": "o", "filter": {"k": "a"}}, S2)),
    (
        "match $jsonSchema enum",
        match({"$jsonSchema": {"properties": {"s": {"enum": ["A"]}}}, "s": {"$type": "string"}}),
    ),
    ("match $mod", match({"n": {"$mod": [3, 0]}, "s": "A"})),
    ("match $bitsAllSet", match({"s": {"$bitsAllSet": 1}})),
    ("match $comment", match({"s": "A", "$comment": "x"})),
    ("match $alwaysTrue", match({"$alwaysTrue": 1, "s": "Z"})),
    ("match $expr $max", match({"$expr": {"$eq": [{"$max": ["$s", "zz"]}, "ZZ"]}})),
    ("$match stage", agg({"$match": {"s": "A"}}, {"$project": ONLY_ID}, {"$sort": ONLY_ID})),
    (
        "$match after $project",
        agg({"$project": {"t": "$s"}}, {"$match": {"t": "A"}}, {"$sort": ONLY_ID}),
    ),
    (
        "$group $max",
        agg({"$match": {"s": {"$type": "string"}}}, {"$group": {"_id": None, "r": {"$max": "$s"}}}),
    ),
    (
        "$group $min",
        agg(
            {"$match": {"s": {"$in": ["b", "B", "z"]}}},
            {"$group": {"_id": None, "r": {"$min": "$s"}}},
        ),
    ),
    (
        "$group $addToSet ~",
        agg(
            {"$match": {"s": {"$in": ["a", "b"]}}},
            {"$group": {"_id": None, "r": {"$addToSet": "$s"}}},
            {"$unwind": "$r"},
            {"$project": {"_id": 0}},
        ),
    ),
    (
        "$group $push",
        agg(
            {"$match": {"s": {"$in": ["a", "b"]}}},
            {"$sort": ONLY_ID},
            {"$group": {"_id": None, "r": {"$push": "$s"}}},
        ),
    ),
    (
        "$group compound key ~",
        agg(
            {"$match": {"s": {"$in": ["a", "b"]}}},
            {"$group": {"_id": {"s": "$s", "n": "$absent"}, "c": {"$sum": 1}}},
            {"$project": {"_id": 0}},
        ),
    ),
    (
        "$group array key ~",
        agg(
            {"$match": {"s": {"$in": ["a", "b"]}}},
            {"$group": {"_id": ["$s"], "c": {"$sum": 1}}},
            {"$project": {"_id": 0}},
        ),
    ),
    (
        "$group $top",
        agg(
            {"$match": {"s": {"$type": "string"}}},
            {
                "$group": {
                    "_id": None,
                    "r": {"$top": {"sortBy": {"s": -1, "_id": 1}, "output": "$_id"}},
                }
            },
        ),
    ),
    (
        "$group $firstN after $sort",
        agg(
            {"$sort": {"s": 1, "_id": 1}},
            {"$group": {"_id": None, "r": {"$firstN": {"input": "$_id", "n": 6}}}},
        ),
    ),
    (
        "$sortByCount",
        agg(
            {"$match": {"s": {"$in": ["a", "b"]}}}, {"$sortByCount": "$s"}, {"$project": {"_id": 0}}
        ),
    ),
    (
        "$bucket",
        agg(
            {"$match": {"s": {"$type": "string"}}},
            {"$bucket": {"groupBy": "$s", "boundaries": ["", "B", "c"], "default": "other"}},
        ),
    ),
    (
        "$bucketAuto",
        agg(
            {"$match": {"s": {"$in": ["a", "b", "z"]}}},
            {"$bucketAuto": {"groupBy": "$s", "buckets": 2}},
        ),
    ),
    ("$lookup", agg({"$lookup": LOOKUP}, *JOINED)),
    ("$lookup no collation", agg({"$lookup": LOOKUP}, *JOINED, collation=None)),
    ("$lookup pipeline", agg({"$lookup": LOOKUP_PIPE}, *JOINED)),
    ("$lookup s1", agg({"$lookup": LOOKUP}, *JOINED, collation=S1)),
    ("$lookup own collation", agg({"$lookup": {**LOOKUP, "_internalCollation": S1}}, *JOINED)),
    (
        "$graphLookup",
        agg(
            {"$match": {"_id": 0}},
            {
                "$graphLookup": {
                    "from": "o",
                    "startWith": "$s",
                    "connectFromField": "k",
                    "connectToField": "k",
                    "as": "m",
                }
            },
            {"$project": {"m._id": 1}},
        ),
    ),
    (
        "$unionWith",
        agg(
            {"$match": {"s": "Z"}},
            {"$unionWith": {"coll": "o", "pipeline": [{"$match": {"k": "b"}}]}},
            {"$project": ONLY_ID},
        ),
    ),
    (
        "$facet",
        agg({"$facet": {"f": [{"$match": {"s": "Z"}}, {"$project": ONLY_ID}, {"$sort": ONLY_ID}]}}),
    ),
    (
        "$setWindowFields",
        agg(
            {"$match": {"s": {"$in": ["a", "b"]}}},
            {"$setWindowFields": {"sortBy": {"s": 1}, "output": {"r": {"$rank": {}}}}},
            {"$project": {"r": 1}},
            {"$sort": ONLY_ID},
        ),
    ),
    (
        "$setWindowFields array partition",
        agg(
            {"$match": {"_id": 17}},
            {"$setWindowFields": {"partitionBy": "$s", "output": {"r": {"$sum": 1}}}},
        ),
    ),
    (
        "$setWindowFields partition",
        agg(
            {"$match": {"_id": {"$in": [0, 1, 4, 5]}}},
            {
                "$setWindowFields": {
                    "partitionBy": "$s",
                    "sortBy": {"_id": 1},
                    "output": {"r": {"$documentNumber": {}}},
                }
            },
            {"$project": {"r": 1}},
            {"$sort": ONLY_ID},
        ),
    ),
    (
        "$densify string partition",
        agg(
            {"$match": {"_id": {"$in": [0, 1]}}},
            {
                "$densify": {
                    "field": "n",
                    "partitionByFields": ["s"],
                    "range": {"step": 0.5, "bounds": "partition"},
                }
            },
            {"$project": {"_id": 0, "s": 1, "n": 1}},
        ),
    ),
    (
        "$fill partition",
        agg(
            {"$match": {"s": {"$in": ["a"]}}},
            {
                "$fill": {
                    "partitionByFields": ["s"],
                    "sortBy": {"_id": 1},
                    "output": {"q": {"method": "locf"}},
                }
            },
            {"$project": {"q": 1}},
            {"$sort": ONLY_ID},
        ),
    ),
    (
        "$redact",
        agg(
            {"$match": {"_id": {"$lt": 4}}},
            {"$redact": {"$cond": [{"$eq": ["$s", "A"]}, "$$KEEP", "$$PRUNE"]}},
            {"$project": ONLY_ID},
        ),
    ),
    (
        "$replaceWith $setUnion ~",
        agg({"$limit": 1}, {"$replaceWith": {"r": {"$setUnion": [["z", "Z", "á", "a"]]}}}),
    ),
    ("$count after $match", agg({"$match": {"s": {"$lt": "B"}}}, {"$count": "n"})),
    (
        "$sort then $limit",
        agg({"$sort": {"s": -1, "_id": 1}}, {"$limit": 3}, {"$project": ONLY_ID}),
    ),
    (
        "$sort on computed",
        agg(
            {
                "$project": {
                    "t": {
                        "$concat": [
                            {
                                "$ifNull": [
                                    {"$cond": [{"$eq": [{"$type": "$s"}, "string"]}, "$s", "?"]},
                                    "?",
                                ]
                            },
                            "x",
                        ]
                    }
                }
            },
            {"$sort": {"t": 1, "_id": 1}},
            {"$limit": 8},
            {"$project": ONLY_ID},
        ),
    ),
    ("$sort $meta", agg({"$sort": {"s": 1, "_id": -1}}, {"$skip": 20}, {"$project": ONLY_ID})),
    (
        "find sort array field",
        with_collation(
            {
                "find": "c",
                "filter": {"_id": {"$in": [0, 4, 17]}},
                "sort": {"s": -1},
                "projection": ONLY_ID,
            },
            S2,
        ),
    ),
    (
        "find sort document field",
        with_collation(
            {
                "find": "c",
                "filter": {"_id": {"$in": [18, 25, 4]}},
                "sort": {"s": 1},
                "projection": ONLY_ID,
            },
            S2,
        ),
    ),
    (
        "find projection $elemMatch",
        with_collation(
            {"find": "c", "filter": {"_id": 17}, "projection": {"s": {"$elemMatch": {"$eq": "b"}}}},
            S2,
        ),
    ),
    (
        "find positional projection",
        with_collation(
            {"find": "c", "filter": {"_id": 17, "s": "b"}, "projection": {"s.$": 1}}, S2
        ),
    ),
    (
        "distinct on array ~",
        with_collation({"distinct": "c", "key": "s", "query": {"_id": {"$in": [0, 1, 17]}}}, S2),
    ),
    ("count no query", with_collation({"count": "c"}, S2)),
    (
        "count with hint",
        with_collation({"count": "c", "query": {"s": "A"}, "hint": {"_id": 1}}, S2),
    ),
    (
        "explain find",
        {
            "explain": {"find": "c", "filter": {"s": "a"}, "collation": S2},
            "verbosity": "queryPlanner",
        },
    ),
]


def create(name, collation=None, **more):
    return with_collation({"create": name, **more}, collation)


def insert(name, *docs):
    return {"insert": name, "documents": list(docs)}


def index(name, key, collation=None, ix=None, **more):
    spec = with_collation({"key": key, "name": ix or "ix", **more}, collation)
    return {"createIndexes": name, "indexes": [spec]}


def find(name, filter_=None, collation=None, **more):
    return with_collation({"find": name, "filter": filter_ or {}, **more}, collation)


def update(name, q, u, collation=None, **more):
    return {"update": name, "updates": [with_collation({"q": q, "u": u, **more}, collation)]}


def delete(name, q, collation=None, limit=0):
    return {"delete": name, "deletes": [with_collation({"q": q, "limit": limit}, collation)]}


LIST = {"listCollections": 1, "filter": {"name": {"$in": ["t", "v", "u"]}}}
AB = [{"_id": 1, "s": "a"}, {"_id": 2, "s": "A"}, {"_id": 3, "s": "b"}, {"_id": 4, "s": "á"}]
SIMPLE = {"locale": "simple"}
BY_ID = {"_id": 1}
NUM = {"locale": "en", "numericOrdering": True}

STATEFUL = [
    ("default: listCollections", [create("t", S2), LIST]),
    ("default: listIndexes", [create("t", S2), {"listIndexes": "t"}]),
    (
        "default: find inherits",
        [create("t", S2), insert("t", *AB), find("t", {"s": "A"}, sort=BY_ID)],
    ),
    ("default: find simple", [create("t", S2), insert("t", *AB), find("t", {"s": "A"}, SIMPLE)]),
    (
        "default: find another",
        [create("t", S2), insert("t", *AB), find("t", {"s": "A"}, S1, sort=BY_ID)],
    ),
    (
        "default: sort inherits",
        [create("t", S2), insert("t", *AB), find("t", sort={"s": 1, "_id": 1})],
    ),
    (
        "default: _id duplicate",
        [create("t", S2), insert("t", {"_id": "a"}), insert("t", {"_id": "A"}), find("t")],
    ),
    ("default: _id lookup", [create("t", S2), insert("t", {"_id": "a"}), find("t", {"_id": "A"})]),
    (
        "default: _id lookup simple",
        [create("t", S2), insert("t", {"_id": "a"}), find("t", {"_id": "A"}, SIMPLE)],
    ),
    (
        "default: _id update",
        [
            create("t", S2),
            insert("t", {"_id": "a"}),
            update("t", {"_id": "A"}, {"$set": {"x": 1}}),
            find("t"),
        ],
    ),
    (
        "default: upsert _id",
        [
            create("t", S2),
            insert("t", {"_id": "a"}),
            update("t", {"_id": "A"}, {"$set": {"x": 1}}, upsert=True),
            find("t"),
        ],
    ),
    (
        "default: upsert _id simple",
        [
            create("t", S2),
            insert("t", {"_id": "a"}),
            update("t", {"_id": "A"}, {"$set": {"x": 1}}, SIMPLE, upsert=True),
            find("t"),
        ],
    ),
    ("default: index inherits", [create("t", S2), index("t", {"s": 1}), {"listIndexes": "t"}]),
    (
        "default: index simple",
        [create("t", S2), index("t", {"s": 1}, SIMPLE), {"listIndexes": "t"}],
    ),
    (
        "default: unique index",
        [
            create("t", S2),
            index("t", {"s": 1}, unique=True),
            insert("t", *AB),
            find("t", sort=BY_ID),
        ],
    ),
    (
        "default: unique simple index",
        [
            create("t", S2),
            index("t", {"s": 1}, SIMPLE, unique=True),
            insert("t", *AB),
            find("t", sort=BY_ID),
        ],
    ),
    ("default: count", [create("t", S2), insert("t", *AB), {"count": "t", "query": {"s": "A"}}]),
    ("default: distinct ~", [create("t", S2), insert("t", *AB), {"distinct": "t", "key": "s"}]),
    (
        "default: aggregate",
        [
            create("t", S2),
            insert("t", *AB),
            agg({"$match": {"s": "A"}}, {"$sort": BY_ID}, coll="t", collation=None),
        ],
    ),
    (
        "default: aggregate $group ~",
        [
            create("t", S2),
            insert("t", *AB),
            agg(
                {"$group": {"_id": "$s", "n": {"$sum": 1}}},
                {"$project": {"_id": 0}},
                coll="t",
                collation=None,
            ),
        ],
    ),
    (
        "default: update",
        [
            create("t", S2),
            insert("t", *AB),
            update("t", {"s": "A"}, {"$set": {"x": 1}}, multi=True),
            find("t", sort=BY_ID),
        ],
    ),
    (
        "default: delete",
        [create("t", S2), insert("t", *AB), delete("t", {"s": "A"}), find("t", sort=BY_ID)],
    ),
    (
        "default: findAndModify",
        [
            create("t", S2),
            insert("t", *AB),
            {"findAndModify": "t", "query": {"s": "B"}, "update": {"$set": {"x": 1}}, "new": True},
        ],
    ),
    (
        "default: $lookup into plain",
        [
            create("t", S2),
            insert("t", *AB),
            insert("u", {"_id": 9, "k": "A"}),
            agg(
                {"$lookup": {"from": "u", "localField": "s", "foreignField": "k", "as": "m"}},
                {"$project": {"n": {"$size": "$m"}}},
                {"$sort": BY_ID},
                coll="t",
                collation=None,
            ),
        ],
    ),
    (
        "default: $lookup from plain",
        [
            create("t", S2),
            insert("t", *AB),
            insert("u", {"_id": 9, "k": "A"}),
            agg(
                {"$lookup": {"from": "t", "localField": "k", "foreignField": "s", "as": "m"}},
                {"$project": {"n": {"$size": "$m"}}},
                coll="u",
                collation=None,
            ),
        ],
    ),
    (
        "default: $unionWith from plain",
        [
            create("t", S2),
            insert("t", *AB),
            insert("u", {"_id": 9, "k": "A"}),
            agg(
                {"$unionWith": {"coll": "t", "pipeline": [{"$match": {"s": "A"}}]}},
                {"$project": BY_ID},
                coll="u",
                collation=None,
            ),
        ],
    ),
    (
        "default: $out keeps it",
        [create("t", S2), insert("t", *AB), agg({"$out": "u"}, coll="t", collation=None), LIST],
    ),
    (
        "default: $merge into plain",
        [
            create("t", S2),
            insert("t", *AB),
            insert("u", {"_id": 1}),
            agg({"$merge": {"into": "u"}}, coll="t", collation=None),
            find("u", sort=BY_ID),
        ],
    ),
    ("default: collMod", [create("t", S2), {"collMod": "t", "collation": S1}, LIST]),
    ("default: create again same", [create("t", S2), create("t", S2)]),
    ("default: create again other", [create("t", S2), create("t", S1)]),
    ("default: create again none", [create("t", S2), create("t")]),
    (
        "default: rename keeps it",
        [create("t", S2), {"admin": {"renameCollection": f"{DB}.t", "to": f"{DB}.u"}}, LIST],
    ),
    ("default: simple spelled out", [create("t", SIMPLE), LIST, {"listIndexes": "t"}]),
    (
        "default: capped",
        [create("t", S2, capped=True, size=4096), insert("t", *AB), find("t", {"s": "A"})],
    ),
    (
        "default: clustered",
        [
            create("t", S2, clusteredIndex={"key": {"_id": 1}, "unique": True}),
            insert("t", {"_id": "a"}),
            insert("t", {"_id": "A"}),
            find("t", {"_id": "A"}),
        ],
    ),
    (
        "default: validator",
        [
            create("t", S2, validator={"s": "a"}),
            insert("t", {"_id": 1, "s": "A"}),
            insert("t", {"_id": 2, "s": "b"}),
            find("t"),
        ],
    ),
    (
        "default: validator $expr",
        [
            create("t", S2, validator={"$expr": {"$eq": ["$s", "a"]}}),
            insert("t", {"_id": 1, "s": "A"}),
            find("t"),
        ],
    ),
    (
        "default: change stream",
        [
            create("t", S2),
            agg(
                {"$changeStream": {}},
                {"$match": {"operationType": "INSERT"}},
                coll="t",
                collation=None,
            ),
        ],
    ),
    (
        "default: bulkWrite",
        [
            create("t", S2),
            insert("t", *AB),
            {
                "admin": {
                    "bulkWrite": 1,
                    "ops": [
                        {
                            "update": 0,
                            "filter": {"s": "A"},
                            "updateMods": {"$set": {"x": 1}},
                            "multi": True,
                        }
                    ],
                    "nsInfo": [{"ns": f"{DB}.t"}],
                }
            },
            find("t", sort=BY_ID),
        ],
    ),
    (
        "view: inherits nothing",
        [insert("t", *AB), create("v", viewOn="t", pipeline=[]), find("v", {"s": "A"}, sort=BY_ID)],
    ),
    (
        "view: own collation",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[]),
            LIST,
            find("v", {"s": "A"}, sort=BY_ID),
        ],
    ),
    (
        "view: pipeline uses it",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[{"$match": {"s": "A"}}]),
            find("v", sort=BY_ID),
        ],
    ),
    (
        "view: find other collation",
        [insert("t", *AB), create("v", S2, viewOn="t", pipeline=[]), find("v", {"s": "A"}, S1)],
    ),
    (
        "view: find same collation",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[]),
            find("v", {"s": "A"}, S2, sort=BY_ID),
        ],
    ),
    (
        "view: find simple on plain view",
        [insert("t", *AB), create("v", viewOn="t", pipeline=[]), find("v", {"s": "A"}, SIMPLE)],
    ),
    (
        "view: find collation on plain view",
        [insert("t", *AB), create("v", viewOn="t", pipeline=[]), find("v", {"s": "A"}, S2)],
    ),
    (
        "view: on a collated collection",
        [
            create("t", S2),
            insert("t", *AB),
            create("v", viewOn="t", pipeline=[]),
            LIST,
            find("v", {"s": "A"}, sort=BY_ID),
        ],
    ),
    (
        "view: mismatched with collection",
        [create("t", S2), create("v", S1, viewOn="t", pipeline=[]), LIST],
    ),
    (
        "view: on a view, mismatched",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[]),
            create("u", S1, viewOn="v", pipeline=[]),
        ],
    ),
    (
        "view: on a view, none given",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[]),
            create("u", viewOn="v", pipeline=[]),
            find("u", {"s": "A"}),
        ],
    ),
    (
        "view: $lookup from mismatched view",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[]),
            insert("u", {"_id": 9, "k": "A"}),
            agg(
                {"$lookup": {"from": "v", "localField": "k", "foreignField": "s", "as": "m"}},
                coll="u",
                collation=None,
            ),
        ],
    ),
    (
        "view: count and distinct",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[]),
            {"count": "v", "query": {"s": "A"}},
            {"distinct": "v", "key": "s", "collation": S1},
        ],
    ),
    (
        "view: collMod collation",
        [
            insert("t", *AB),
            create("v", S2, viewOn="t", pipeline=[]),
            {"collMod": "v", "viewOn": "t", "pipeline": [], "collation": S1},
            LIST,
        ],
    ),
    ("index: listIndexes", [insert("t", *AB), index("t", {"s": 1}, S2), {"listIndexes": "t"}]),
    (
        "index: every option",
        [
            insert("t", *AB),
            index(
                "t",
                {"s": 1},
                {
                    "locale": "fr",
                    "strength": 1,
                    "caseLevel": True,
                    "caseFirst": "upper",
                    "numericOrdering": True,
                    "alternate": "shifted",
                    "maxVariable": "space",
                    "normalization": True,
                    "backwards": True,
                },
            ),
            {"listIndexes": "t"},
        ],
    ),
    ("index: fr defaults", [index("t", {"s": 1}, {"locale": "fr_CA"}), {"listIndexes": "t"}]),
    ("index: simple", [index("t", {"s": 1}, SIMPLE), {"listIndexes": "t"}]),
    ("index: same again", [index("t", {"s": 1}, S2), index("t", {"s": 1}, S2)]),
    ("index: same name other collation", [index("t", {"s": 1}, S2), index("t", {"s": 1}, S1)]),
    (
        "index: same key other collation",
        [
            index("t", {"s": 1}, S2),
            index("t", {"s": 1}, S1, ix="ix1"),
            index("t", {"s": 1}, ix="plain"),
            {"listIndexes": "t"},
        ],
    ),
    (
        "index: same key same collation other name",
        [index("t", {"s": 1}, S2), index("t", {"s": 1}, S2, ix="ix2")],
    ),
    (
        "index: spelled-out equal collation",
        [index("t", {"s": 1}, S2), index("t", {"s": 1}, {**S2, "caseLevel": False}, ix="ix2")],
    ),
    (
        "index: drop by key when ambiguous",
        [
            index("t", {"s": 1}, S2),
            index("t", {"s": 1}, ix="plain"),
            {"dropIndexes": "t", "index": {"s": 1}},
            {"listIndexes": "t"},
        ],
    ),
    (
        "index: query matching",
        [insert("t", *AB), index("t", {"s": 1}, S2), find("t", {"s": "A"}, S2, sort=BY_ID)],
    ),
    ("index: query without", [insert("t", *AB), index("t", {"s": 1}, S2), find("t", {"s": "A"})]),
    (
        "index: query other",
        [insert("t", *AB), index("t", {"s": 1}, S2), find("t", {"s": "A"}, S1, sort=BY_ID)],
    ),
    (
        "index: hint without collation",
        [insert("t", *AB), index("t", {"s": 1}, S2), find("t", {"s": "A"}, hint="ix")],
    ),
    (
        "index: hint other collation",
        [
            insert("t", *AB),
            index("t", {"s": 1}, S2),
            find("t", {"s": "A"}, S1, hint="ix", sort=BY_ID),
        ],
    ),
    (
        "index: range matching",
        [
            insert("t", *AB),
            index("t", {"s": 1}, S2),
            find("t", {"s": {"$gte": "A", "$lt": "B"}}, S2, sort=BY_ID),
        ],
    ),
    (
        "index: sort matching",
        [
            insert("t", *AB),
            index("t", {"s": 1}, S2),
            find("t", collation=S2, sort={"s": -1, "_id": 1}),
        ],
    ),
    (
        "index: sort without",
        [insert("t", *AB), index("t", {"s": 1}, S2), find("t", sort={"s": 1, "_id": 1}, hint="ix")],
    ),
    (
        "index: unique",
        [index("t", {"s": 1}, S2, unique=True), insert("t", *AB), find("t", sort=BY_ID)],
    ),
    (
        "index: unique s1",
        [index("t", {"s": 1}, S1, unique=True), insert("t", *AB), find("t", sort=BY_ID)],
    ),
    (
        "index: unique build over duplicates",
        [insert("t", *AB), index("t", {"s": 1}, S2, unique=True), {"listIndexes": "t"}],
    ),
    (
        "index: unique update into duplicate",
        [
            index("t", {"s": 1}, S2, unique=True),
            insert("t", AB[0], AB[2]),
            update("t", {"_id": 3}, {"$set": {"s": "A"}}),
            find("t", sort=BY_ID),
        ],
    ),
    (
        "index: unique in documents",
        [
            index("t", {"s": 1}, S2, unique=True),
            insert("t", {"_id": 1, "s": {"k": "a"}}),
            insert("t", {"_id": 2, "s": {"k": "A"}}),
            insert("t", {"_id": 3, "s": ["x", "X"]}),
            insert("t", {"_id": 4, "s": ["x"]}),
            find("t", sort=BY_ID),
        ],
    ),
    (
        "index: unique numeric",
        [
            index("t", {"s": 1}, NUM, unique=True),
            insert("t", {"_id": 1, "s": "10"}),
            insert("t", {"_id": 2, "s": "010"}),
            insert("t", {"_id": 3, "s": "9"}),
            find("t", collation=NUM, sort={"s": 1}),
        ],
    ),
    (
        "index: numeric range",
        [
            insert(
                "t",
                *[{"_id": i, "s": v} for i, v in enumerate(["10", "9", "2", "a10", "a9", "100"])],
            ),
            index("t", {"s": 1}, NUM),
            find("t", {"s": {"$gt": "9"}}, NUM, sort=BY_ID),
            find("t", collation=NUM, sort={"s": 1}),
        ],
    ),
    (
        "index: compound",
        [
            insert("t", *AB),
            index("t", {"s": 1, "_id": -1}, S2),
            find("t", {"s": "A", "_id": {"$gt": 1}}, S2),
            find("t", collation=S2, sort={"s": 1, "_id": -1}),
        ],
    ),
    (
        "index: partial filter uses collation",
        [
            insert("t", *AB),
            index("t", {"_id": 1, "s": 1}, S2, partialFilterExpression={"s": {"$gte": "A"}}),
            find("t", {"s": {"$gte": "A"}}, S2, sort=BY_ID),
            find("t", {"s": {"$gte": "b"}}, S2, hint="ix"),
        ],
    ),
    (
        "index: unique partial",
        [
            index("t", {"s": 1}, S2, unique=True, partialFilterExpression={"s": "a"}),
            insert("t", {"_id": 1, "s": "a"}),
            insert("t", {"_id": 2, "s": "A"}),
            insert("t", {"_id": 3, "s": "b"}),
            insert("t", {"_id": 4, "s": "B"}),
            find("t", sort=BY_ID),
        ],
    ),
    (
        "index: sparse",
        [
            insert("t", *AB, {"_id": 9}),
            index("t", {"s": 1}, S2, sparse=True),
            find("t", {"s": "A"}, S2, sort=BY_ID),
        ],
    ),
    ("index: hashed", [index("t", {"s": "hashed"}, S2)]),
    ("index: text", [index("t", {"s": "text"}, S2)]),
    ("index: 2dsphere", [index("t", {"g": "2dsphere", "s": 1}, S2), {"listIndexes": "t"}]),
    ("index: wildcard", [index("t", {"$**": 1}, S2)]),
    ("index: on _id", [index("t", {"_id": 1}, S2), {"listIndexes": "t"}]),
    ("index: on _id, other name", [index("t", {"_id": 1}, S2, ix="other"), {"listIndexes": "t"}]),
    (
        "index: ttl with collation",
        [index("t", {"s": 1}, S2, expireAfterSeconds=5), {"listIndexes": "t"}],
    ),
    (
        "index: count and distinct ~",
        [
            insert("t", *AB),
            index("t", {"s": 1}, S2),
            {"count": "t", "query": {"s": "A"}, "collation": S2},
            {"distinct": "t", "key": "s", "collation": S2},
        ],
    ),
    (
        "index: $lookup uses it",
        [
            insert("t", *AB),
            index("t", {"s": 1}, S2),
            insert("u", {"_id": 9, "k": "A"}),
            agg(
                {"$lookup": {"from": "t", "localField": "k", "foreignField": "s", "as": "m"}},
                {"$project": {"m._id": 1}},
                coll="u",
            ),
        ],
    ),
    (
        "index: collMod",
        [index("t", {"s": 1}, S2), {"collMod": "t", "index": {"name": "ix", "collation": S1}}],
    ),
    (
        "write: update multi",
        [
            insert("t", *AB),
            update("t", {"s": "A"}, {"$set": {"x": 1}}, S2, multi=True),
            find("t", sort=BY_ID),
        ],
    ),
    (
        "write: update one",
        [insert("t", *AB), update("t", {"s": "A"}, {"$set": {"x": 1}}, S2), find("t", sort=BY_ID)],
    ),
    (
        "write: update no-op set",
        [
            insert("t", *AB),
            update("t", {"_id": 1}, {"$set": {"s": "A"}}, S2),
            find("t", {"_id": 1}),
        ],
    ),
    (
        "write: upsert seeds",
        [
            insert("t", *AB),
            update("t", {"s": "Z"}, {"$set": {"x": 1}}, S2, upsert=True),
            find("t", {"x": 1}, projection={"_id": 0}),
        ],
    ),
    (
        "write: upsert matches",
        [
            insert("t", *AB),
            update("t", {"s": "B"}, {"$set": {"x": 1}}, S2, upsert=True),
            find("t", sort=BY_ID),
        ],
    ),
    (
        "write: $pull",
        [
            insert("t", {"_id": 1, "a": ["a", "A", "b", "á"]}),
            update("t", {}, {"$pull": {"a": "A"}}, S2),
            find("t"),
        ],
    ),
    (
        "write: $pull condition",
        [
            insert("t", {"_id": 1, "a": ["a", "A", "b", "B"]}),
            update("t", {}, {"$pull": {"a": {"$gte": "B"}}}, S2),
            find("t"),
        ],
    ),
    (
        "write: $pull document",
        [
            insert("t", {"_id": 1, "a": [{"k": "a"}, {"k": "B"}]}),
            update("t", {}, {"$pull": {"a": {"k": "A"}}}, S2),
            find("t"),
        ],
    ),
    (
        "write: $pullAll",
        [
            insert("t", {"_id": 1, "a": ["a", "A", "b"]}),
            update("t", {}, {"$pullAll": {"a": ["A"]}}, S2),
            find("t"),
        ],
    ),
    (
        "write: $addToSet",
        [
            insert("t", {"_id": 1, "a": ["a", "b"]}),
            update("t", {}, {"$addToSet": {"a": "A"}}, S2),
            find("t"),
        ],
    ),
    (
        "write: $addToSet $each",
        [
            insert("t", {"_id": 1, "a": ["a"]}),
            update("t", {}, {"$addToSet": {"a": {"$each": ["A", "b", "B"]}}}, S2),
            find("t"),
        ],
    ),
    (
        "write: $push $sort",
        [
            insert("t", {"_id": 1, "a": ["b", "A"]}),
            update("t", {}, {"$push": {"a": {"$each": ["a", "B"], "$sort": 1}}}, S2),
            find("t"),
        ],
    ),
    (
        "write: $push $sort by field",
        [
            insert("t", {"_id": 1, "a": [{"k": "b"}, {"k": "A"}]}),
            update(
                "t",
                {},
                {"$push": {"a": {"$each": [{"k": "a"}, {"k": "B"}], "$sort": {"k": -1}}}},
                S2,
            ),
            find("t"),
        ],
    ),
    (
        "write: $max",
        [insert("t", {"_id": 1, "s": "a"}), update("t", {}, {"$max": {"s": "A"}}, S2), find("t")],
    ),
    (
        "write: $max larger",
        [insert("t", {"_id": 1, "s": "a"}), update("t", {}, {"$max": {"s": "B"}}, S2), find("t")],
    ),
    (
        "write: $min",
        [insert("t", {"_id": 1, "s": "b"}), update("t", {}, {"$min": {"s": "A"}}, S2), find("t")],
    ),
    (
        "write: $min no collation",
        [insert("t", {"_id": 1, "s": "b"}), update("t", {}, {"$min": {"s": "A"}}), find("t")],
    ),
    (
        "write: arrayFilters",
        [
            insert("t", {"_id": 1, "a": ["a", "A", "b"]}),
            update("t", {}, {"$set": {"a.$[e]": "x"}}, S2, arrayFilters=[{"e": "A"}]),
            find("t"),
        ],
    ),
    (
        "write: positional",
        [
            insert("t", {"_id": 1, "a": ["b", "A", "a"]}),
            update("t", {"a": "a"}, {"$set": {"a.$": "x"}}, S2),
            find("t"),
        ],
    ),
    (
        "write: pipeline update",
        [
            insert("t", *AB),
            update("t", {}, [{"$set": {"x": {"$eq": ["$s", "A"]}}}], S2, multi=True),
            find("t", sort=BY_ID),
        ],
    ),
    ("write: delete many", [insert("t", *AB), delete("t", {"s": "A"}, S2), find("t", sort=BY_ID)]),
    (
        "write: delete one",
        [insert("t", *AB), delete("t", {"s": "A"}, S2, limit=1), find("t", sort=BY_ID)],
    ),
    ("write: delete s1", [insert("t", *AB), delete("t", {"s": "A"}, S1), find("t", sort=BY_ID)]),
    (
        "write: findAndModify sort",
        [
            insert("t", *AB),
            {
                "findAndModify": "t",
                "query": {},
                "sort": {"s": -1, "_id": -1},
                "remove": True,
                "collation": S2,
            },
        ],
    ),
    (
        "write: findAndModify query",
        [
            insert("t", *AB),
            {
                "findAndModify": "t",
                "query": {"s": "B"},
                "update": {"$set": {"x": 1}},
                "new": True,
                "collation": S2,
            },
        ],
    ),
    (
        "write: findAndModify upsert",
        [
            insert("t", *AB),
            {
                "findAndModify": "t",
                "query": {"s": "A"},
                "update": {"$set": {"x": 1}},
                "upsert": True,
                "new": True,
                "collation": S1,
                "sort": {"_id": -1},
            },
        ],
    ),
    (
        "write: bulkWrite",
        [
            insert("t", *AB),
            {
                "admin": {
                    "bulkWrite": 1,
                    "ops": [
                        {
                            "update": 0,
                            "filter": {"s": "A"},
                            "updateMods": {"$set": {"x": 1}},
                            "multi": True,
                            "collation": S2,
                        },
                        {"delete": 0, "filter": {"s": "B"}, "collation": S2},
                    ],
                    "nsInfo": [{"ns": f"{DB}.t"}],
                }
            },
            find("t", sort=BY_ID),
        ],
    ),
    (
        "write: update with a collated unique index",
        [
            index("t", {"s": 1}, S2, unique=True),
            insert("t", {"_id": 1, "s": "a"}),
            update("t", {"_id": 1}, {"$set": {"s": "A"}}),
            find("t"),
        ],
    ),
    (
        "write: replace changes case only",
        [insert("t", {"_id": 1, "s": "a"}), update("t", {"s": "A"}, {"s": "A"}, S2), find("t")],
    ),
    (
        "write: $out with collation",
        [
            insert("t", *AB),
            agg({"$match": {"s": "A"}}, {"$out": "u"}, coll="t"),
            find("u", sort=BY_ID),
            LIST,
        ],
    ),
    (
        "write: $merge on collated key",
        [
            insert("t", *AB),
            index("u", {"s": 1}, S2, unique=True),
            agg({"$project": {"_id": 0, "s": 1}}, {"$merge": {"into": "u", "on": "s"}}, coll="t"),
        ],
    ),
    (
        "change stream: collation on the pipeline",
        [insert("t", *AB), agg({"$changeStream": {}}, coll="t")],
    ),
]

SCENARIOS = (
    [(f"read {label}", [command]) for label, command in reads()]
    + [(f"spec {label}", c if isinstance(c, list) else [c]) for label, c in specs()]
    + [(f"expr {label}", [command]) for label, command in EXPRESSIONS]
    + [(f"state {label}", commands) for label, commands in STATEFUL]
)


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
    seeded = False
    for label, commands in SCENARIOS:
        fresh = label.startswith("state ")
        if fresh or not seeded:
            client.drop_database(DB)
            seeded = False
        if not fresh and not seeded:
            seed(client[DB])
            seeded = True
        bag = label.endswith("~")
        results[label] = [outcome(client, command, bag) for command in commands]
    return results


def self_check(expected):
    """The probe is worthless unless mongod itself tells `a` from `A` and, at
    strength 2, does not."""
    plain = expected["read none: eq a"][0]
    folded = expected["read s2: eq a"][0]
    ids = lambda *n: ("OK", [{"_id": f"int:{i}"} for i in n])  # noqa: E731
    if plain != ids(0, 17) or folded != ids(0, 1, 17):
        sys.exit(f"SELF-CHECK FAILED on the reference server: {plain} {folded}")


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
        return report("collation", len(SCENARIOS), divergent)


if __name__ == "__main__":
    sys.exit(main())
