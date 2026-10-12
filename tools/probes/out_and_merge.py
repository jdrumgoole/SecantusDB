"""Differential-probe `$out` and `$merge` against a real mongod.

What the two writing stages accept, and what they leave behind: for every
case the whole reply is compared AND the documents of the source and of the
target afterwards, in the order each collection holds them, with the
target's indexes and options. A stage that answers `ok` and leaves a
different collection behind is the failure this is here to find, and the
reply alone does not show it.

A `case` runs against collections of its own: `c` (the source), `t` (the
target) and `v` are dropped, the `setup` commands run, the seed documents
are inserted, the command runs, and both collections are read back.
Commands go over a raw connection, so an argument a driver would refuse to
send still reaches the server.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/out_and_merge.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import datetime
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Decimal128, Int64, ObjectId, Timestamp  # noqa: E402
from bson.errors import InvalidBSON  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
RUN = f"{os.getpid() % 100000}"

DROP = {"$clusterTime", "operationTime", "opTime", "electionId"}

BAD = ["x", 2.5, -1, None, True, {}, [1], Int64(2), 0, Decimal128("2"), 1]
OID = ObjectId("0123456789abcdef01234567")

S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


def case(source, target, pipeline, setup=(), **top):
    """Run `pipeline` over `source`, with `target` already holding documents."""
    command = {"aggregate": "c", "pipeline": pipeline, "cursor": {}, **top}
    return (source, target, command, list(setup))


def merge(spec, source=None, target=None, before=(), setup=(), **top):
    rows = SOURCE if source is None else source
    held = TARGET if target is None else target
    return case(rows, held, [*before, {"$merge": spec}], setup, **top)


def out(spec, source=None, target=None, before=(), setup=(), **top):
    rows = SOURCE if source is None else source
    held = TARGET if target is None else target
    return case(rows, held, [*before, {"$out": spec}], setup, **top)


def index(key, coll="t", **options):
    name = options.pop("name", "_".join(f"{k}_{v}" for k, v in key.items()))
    return {"createIndexes": coll, "indexes": [{"key": key, "name": name, **options}]}


SOURCE = [
    {"_id": 1, "a": 10, "k": "x", "o": {"p": 1}},
    {"_id": 2, "a": 20, "k": "y", "o": {"q": 2}},
    {"_id": 3, "a": 30, "k": "z"},
]
TARGET = [
    {"_id": 2, "b": 2, "k": "y", "o": {"p": 7, "r": 8}},
    {"_id": 9, "b": 9, "k": "w"},
    {"_id": 1, "b": 1, "k": "x", "a": 0},
]

# -------------------------------------------------------------------- $merge

scenario(
    "$merge: the spec",
    *[merge(v) for v in BAD],
    merge("t"),
    merge({"into": "t"}),
    merge({}),
    merge({"into": "t", "nope": 1}),
    merge({"into": ""}),
    merge({"into": {"coll": "t"}}),
    merge({"into": {"db": "x"}}),
    merge({"into": {"db": "", "coll": "t"}}),
    merge({"into": {"coll": ""}}),
    merge({"into": {"coll": "t", "nope": 1}}),
    merge({"into": {}}),
    *[merge({"into": v}) for v in [1, None, [], True, 2.5]],
    *[merge({"into": {"coll": v}}) for v in [1, None, [], {}]],
    *[merge({"into": {"db": v, "coll": "t"}}) for v in [1, None, [], {}]],
    *[
        merge({"into": name})
        for name in ["a$b", "$a", ".a", "a.", "system.x", "system.views", "x" * 256, "c", "new"]
    ],
    merge("new"),
    merge("c"),
)
MATCHED = ["merge", "replace", "keepExisting", "fail", [{"$set": {"m": "$$new.a"}}]]
NOT_MATCHED = ["insert", "discard", "fail"]
scenario(
    "$merge: every mode",
    *[
        merge({"into": "t", "whenMatched": matched, "whenNotMatched": absent})
        for matched in MATCHED
        for absent in NOT_MATCHED
    ],
    *[
        merge({"into": "t", "whenMatched": matched, "whenNotMatched": absent}, target=[])
        for matched in MATCHED
        for absent in NOT_MATCHED
    ],
)
scenario(
    "$merge: whenMatched and whenNotMatched",
    *[
        merge({"into": "t", "whenMatched": v})
        for v in [*BAD, "nope", "", "Merge", "delete", "insert"]
    ],
    *[
        merge({"into": "t", "whenNotMatched": v})
        for v in [*BAD, "nope", "", "Insert", "merge", "replace", "keepExisting"]
    ],
    merge({"into": "t", "whenMatched": None, "whenNotMatched": None}),
)
scenario(
    "$merge: what merge and replace store",
    merge("t", source=[{"_id": 1, "o": {"p": 1}}], target=[{"_id": 1, "o": {"q": 2}, "z": 1}]),
    merge("t", source=[{"_id": 1, "z": 5, "a": 1}], target=[{"_id": 1, "y": 1, "z": 1}]),
    merge("t", source=[{"_id": 1, "l": [1]}], target=[{"_id": 1, "l": [2, 3]}]),
    merge("t", source=[{"_id": 1, "o": None}], target=[{"_id": 1, "o": {"q": 2}}]),
    merge("t", source=[{"_id": 1}], target=[{"_id": 1, "y": 1}]),
    merge("t", source=[{"_id": 1.0, "a": 1}], target=[{"_id": 1, "y": 1}]),
    merge("t", source=[{"_id": Int64(1), "a": 1}], target=[{"_id": 1, "y": 1}]),
    merge(
        {"into": "t", "whenMatched": "replace"},
        source=[{"_id": 1.0, "a": 1}],
        target=[{"_id": 1, "y": 1}],
    ),
    merge(
        {"into": "t", "whenMatched": "replace"},
        source=[{"_id": 2, "z": 5, "a": 1}],
        target=[{"_id": 1}, {"_id": 2, "y": 1}, {"_id": 3}],
    ),
    merge(
        {"into": "t", "whenMatched": "replace"},
        source=[{"_id": {"a": 1.0}, "n": 1}],
        target=[{"_id": {"a": 1}, "y": 1}],
    ),
    merge("t", source=[{"_id": {"a": 1.0}, "n": 1}], target=[{"_id": {"a": 1}, "y": 1}]),
    merge(
        {"into": "t", "whenMatched": "keepExisting"},
        source=[{"_id": 1, "a": 1}, {"_id": 4, "a": 4}],
        target=[{"_id": 1, "y": 1}],
    ),
    merge(
        "t",
        target=[],
        before=[{"$project": {"_id": {"$literal": 5}, "a": 1}}],
    ),
    merge(
        {"into": "t", "whenMatched": "fail"},
        target=[],
        before=[{"$project": {"_id": {"$literal": 5}, "a": 1}}],
    ),
    merge(
        {"into": "t", "whenMatched": "replace"},
        target=[],
        before=[{"$project": {"_id": {"$literal": 5}, "a": 1}}],
    ),
)
scenario(
    "$merge: a whenMatched pipeline",
    *[
        merge({"into": "t", "whenMatched": pipeline})
        for pipeline in [
            [],
            [{"$set": {"m": 1}}],
            [{"$addFields": {"m": "$$new"}}],
            [{"$replaceWith": "$$new"}],
            [{"$replaceRoot": {"newRoot": "$$new"}}],
            [{"$replaceWith": {"$mergeObjects": ["$$new", "$$ROOT"]}}],
            [{"$unset": "b"}],
            [{"$project": {"b": 1}}],
            [{"$project": {"_id": 0, "b": 1}}],
            [{"$set": {"_id": 99}}],
            [{"$set": {"_id": "$$new._id"}}],
            [{"$replaceWith": {"x": 1}}],
            [{"$match": {"b": 1}}],
            [{"$limit": 1}],
            [{"$set": {"m": "$$nope"}}],
            [{"$set": {"m": {"$nope": 1}}}],
            [{"$nope": 1}],
            [1],
            [{"$set": {"m": 1}}, {"$unset": "k"}],
            [{"$set": {"m": {"$add": ["$b", "$$new.a"]}}}],
            [{"$set": {"m": "$$ROOT"}}],
            [{"$set": {"m": "$$NOW"}}, {"$set": {"m": {"$type": "$m"}}}],
        ]
    ],
)
scenario(
    "$merge: let",
    merge({"into": "t", "let": {"v": 5}, "whenMatched": [{"$set": {"m": "$$v"}}]}),
    merge({"into": "t", "let": {"v": "$a"}, "whenMatched": [{"$set": {"m": "$$v"}}]}),
    merge({"into": "t", "let": {"v": "$a"}, "whenMatched": [{"$set": {"m": "$$new"}}]}),
    merge({"into": "t", "let": {"new": "$a"}, "whenMatched": [{"$set": {"m": "$$new"}}]}),
    merge({"into": "t", "let": {}, "whenMatched": [{"$set": {"m": "$$new.a"}}]}),
    merge({"into": "t", "let": {"v": 5}}),
    merge({"into": "t", "let": {"v": 5}, "whenMatched": "replace"}),
    merge({"into": "t", "let": {"V": 5}, "whenMatched": [{"$set": {"m": 1}}]}),
    merge({"into": "t", "let": {"v": {"$nope": 1}}, "whenMatched": [{"$set": {"m": 1}}]}),
    *[
        merge({"into": "t", "let": v, "whenMatched": [{"$set": {"m": 1}}]})
        for v in [1, "x", [], None]
    ],
    merge(
        {"into": "t", "whenMatched": [{"$set": {"m": "$$top"}}]},
        let={"top": 7},
    ),
    merge(
        {"into": "t", "let": {"v": "$$top"}, "whenMatched": [{"$set": {"m": "$$v"}}]},
        let={"top": 7},
    ),
)
scenario(
    "$merge: on",
    *[merge({"into": "t", "on": v}) for v in [1, None, {}, True, 2.5, [], [1], ["k", 1], ""]],
    merge({"into": "t", "on": "_id"}),
    merge({"into": "t", "on": ["_id"]}),
    merge({"into": "t", "on": "k"}),
    merge({"into": "t", "on": "k"}, setup=[index({"k": 1})]),
    merge({"into": "t", "on": "k"}, setup=[index({"k": 1}, unique=True)]),
    merge({"into": "t", "on": ["k"]}, setup=[index({"k": 1}, unique=True)]),
    merge({"into": "t", "on": "k"}, setup=[index({"k": -1}, unique=True)]),
    merge({"into": "t", "on": "k"}, setup=[index({"k": 1}, unique=True, sparse=True)]),
    merge(
        {"into": "t", "on": "k"},
        setup=[index({"k": 1}, unique=True, partialFilterExpression={"b": {"$gt": 0}})],
    ),
    merge(
        {"into": "t", "on": "k"},
        setup=[index({"k": 1}, unique=True, collation={"locale": "en", "strength": 2})],
    ),
    merge({"into": "t", "on": ["k", "_id"]}, setup=[index({"k": 1, "_id": 1}, unique=True)]),
    merge({"into": "t", "on": ["_id", "k"]}, setup=[index({"k": 1, "_id": 1}, unique=True)]),
    merge({"into": "t", "on": ["k", "a"]}, setup=[index({"k": 1}, unique=True)]),
    merge({"into": "t", "on": ["k"]}, setup=[index({"k": 1, "a": 1}, unique=True)]),
    merge({"into": "t", "on": ["k", "k"]}, setup=[index({"k": 1}, unique=True)]),
    merge(
        {"into": "t", "on": "o.p"},
        target=[{"_id": 2, "o": {"p": 1}, "b": 2}],
        setup=[index({"o.p": 1}, unique=True)],
    ),
    merge({"into": "new", "on": "k"}),
    merge({"into": "t", "on": "k"}, target=[]),
    merge(
        {"into": "t", "on": "k", "whenMatched": "replace"},
        setup=[index({"k": 1}, unique=True)],
    ),
    merge(
        {"into": "t", "on": "k", "whenMatched": "replace"},
        before=[{"$project": {"_id": 0}}],
        setup=[index({"k": 1}, unique=True)],
    ),
    merge(
        {"into": "t", "on": "k"},
        before=[{"$project": {"_id": 0}}],
        setup=[index({"k": 1}, unique=True)],
    ),
    merge(
        {"into": "t", "on": "k", "whenMatched": [{"$set": {"m": "$$new.a"}}]},
        setup=[index({"k": 1}, unique=True)],
    ),
)
scenario(
    "$merge: a document the on fields cannot match with",
    *[
        merge(
            {"into": "t", "on": "k"},
            source=[{"_id": 5, **doc}],
            setup=[index({"k": 1}, unique=True)],
        )
        for doc in [
            {},
            {"k": None},
            {"k": [1, 2]},
            {"k": []},
            {"k": {"a": 1}},
            {"k": 1.0},
            {"k": "x"},
        ]
    ],
    merge("t", before=[{"$project": {"_id": 0}}]),
    merge("t", before=[{"$project": {"_id": 0}}], target=[]),
    merge({"into": "t", "whenNotMatched": "fail"}, before=[{"$project": {"_id": 0}}]),
    merge({"into": "t", "whenNotMatched": "discard"}, before=[{"$project": {"_id": 0}}]),
    merge("t", before=[{"$set": {"_id": None}}], target=[]),
    merge("t", before=[{"$set": {"_id": [1]}}], target=[]),
    merge("t", before=[{"$set": {"_id": {"$literal": {"$x": 1}}}}], target=[]),
    merge(
        {"into": "t", "on": ["k", "a"]},
        source=[{"_id": 5, "k": "x"}],
        setup=[index({"k": 1, "a": 1}, unique=True)],
    ),
)
scenario(
    "$merge: a write the target refuses",
    merge("t", setup=[index({"a": 1}, unique=True)], target=[{"_id": 7, "a": 30}]),
    merge(
        {"into": "t", "whenMatched": "replace"},
        setup=[index({"a": 1}, unique=True)],
        target=[{"_id": 1, "a": 5}, {"_id": 7, "a": 10}],
    ),
    merge(
        {"into": "t", "whenMatched": "merge"},
        setup=[index({"a": 1}, unique=True)],
        target=[{"_id": 1, "a": 5}, {"_id": 7, "a": 10}],
    ),
    merge(
        {"into": "t", "whenMatched": [{"$set": {"a": "$$new.a"}}]},
        setup=[index({"a": 1}, unique=True)],
        target=[{"_id": 1, "a": 5}, {"_id": 7, "a": 10}],
    ),
    merge(
        {"into": "t", "whenMatched": "replace"},
        setup=[index({"a": 1}, unique=True)],
        target=[{"_id": 2, "a": 5}, {"_id": 7, "a": 20}, {"_id": 3, "a": 1}],
    ),
    merge("t", target=[], setup=[{"create": "t", "validator": {"a": {"$gt": 15}}}]),
    merge(
        "t",
        target=[],
        setup=[{"create": "t", "validator": {"a": {"$gt": 15}}}],
        bypassDocumentValidation=True,
    ),
    merge(
        {"into": "t", "whenMatched": "replace"},
        target=[],
        setup=[
            {"create": "t", "validator": {"a": {"$gt": 15}}},
            {"insert": "t", "documents": [{"_id": 1, "a": 99}, {"_id": 2, "a": 98}]},
        ],
    ),
    merge(
        {"into": "t", "whenMatched": "merge"},
        target=[],
        setup=[
            {"create": "t", "validator": {"z": {"$exists": False}}},
            {"insert": "t", "documents": [{"_id": 1, "a": 99}]},
        ],
        before=[{"$set": {"z": 1}}],
    ),
    merge("t", target=[], setup=[{"create": "t", "capped": True, "size": 4096, "max": 2}]),
    merge("v", setup=[{"create": "v", "viewOn": "t", "pipeline": []}]),
    merge("t", before=[{"$set": {"big": {"$literal": {"$x": 1}}}}], target=[]),
    merge("t", before=[{"$replaceWith": {"$literal": {"_id": 1, "a.b": 1, "$c": 2}}}], target=[]),
)
scenario(
    "$merge: where the stage may stand",
    case(SOURCE, TARGET, [{"$merge": "t"}, {"$match": {}}]),
    case(SOURCE, TARGET, [{"$merge": "t"}, {"$merge": "t"}]),
    case(SOURCE, TARGET, [{"$merge": "t"}, {"$out": "t"}]),
    case(SOURCE, TARGET, [{"$facet": {"x": [{"$merge": "t"}]}}]),
    case(SOURCE, TARGET, [{"$lookup": {"from": "t", "pipeline": [{"$merge": "t"}], "as": "x"}}]),
    case(SOURCE, TARGET, [{"$unionWith": {"coll": "t", "pipeline": [{"$merge": "t"}]}}]),
    case(SOURCE, TARGET, [{"$match": {"_id": 99}}, {"$merge": "t"}]),
    case(SOURCE, [], [{"$match": {"_id": 99}}, {"$merge": "new"}]),
    case([], TARGET, [{"$merge": "t"}]),
    case([], [], [{"$merge": "new"}]),
    case(SOURCE, TARGET, [{"$merge": "t"}], cursor={"batchSize": 0}),
    case(SOURCE, TARGET, [{"$merge": "t"}], writeConcern={"w": 1}),
    case(SOURCE, TARGET, [{"$merge": "t"}], readConcern={"level": "majority"}),
    case(SOURCE, TARGET, [{"$merge": "t"}], readConcern={"level": "linearizable"}),
    case(SOURCE, TARGET, [{"$merge": "t"}], readConcern={"level": "snapshot"}),
    case(SOURCE, TARGET, [{"$merge": "t"}], allowDiskUse=True, comment="x"),
    case(SOURCE, TARGET, [{"$sort": {"a": -1}}, {"$limit": 2}, {"$merge": "t"}]),
    case(
        SOURCE,
        TARGET,
        [{"$group": {"_id": "$k", "n": {"$sum": "$a"}}}, {"$sort": {"_id": 1}}, {"$merge": "t"}],
    ),
    case(SOURCE, [], [{"$merge": {"into": "c", "whenMatched": "replace"}}]),
    case(SOURCE, [], [{"$set": {"a": {"$add": ["$a", 1]}}}, {"$merge": "c"}]),
    case(
        SOURCE,
        [],
        [{"$set": {"_id": {"$add": ["$_id", 10]}}}, {"$merge": "c"}],
    ),
    case(SOURCE, TARGET, [{"$merge": "t"}], collation={"locale": "en", "strength": 2}),
)
scenario(
    "$merge: a collation",
    merge(
        {"into": "t", "on": "k"},
        source=[{"_id": 5, "k": "X", "a": 1}],
        target=[],
        setup=[
            {"create": "t", "collation": {"locale": "en", "strength": 2}},
            index({"k": 1}, unique=True),
            {"insert": "t", "documents": [{"_id": 1, "k": "x"}]},
        ],
    ),
    merge(
        {"into": "t"},
        source=[{"_id": "A", "a": 1}],
        target=[],
        setup=[
            {"create": "t", "collation": {"locale": "en", "strength": 2}},
            {"insert": "t", "documents": [{"_id": "a", "y": 1}]},
        ],
    ),
    merge(
        {"into": "t", "whenMatched": "replace"},
        source=[{"_id": "A", "a": 1}],
        target=[],
        setup=[
            {"create": "t", "collation": {"locale": "en", "strength": 2}},
            {"insert": "t", "documents": [{"_id": "a", "y": 1}]},
        ],
    ),
)

# ---------------------------------------------------------------------- $out

scenario(
    "$out: the spec",
    *[out(v) for v in BAD],
    out("t"),
    out({"db": "@self", "coll": "t"}),
    out({"coll": "t"}),
    out({"db": "@self"}),
    out({"db": "@self", "coll": "t", "nope": 1}),
    out({"db": "", "coll": "t"}),
    out({"db": "@self", "coll": ""}),
    out({"db": 1, "coll": "t"}),
    out({"db": "@self", "coll": 1}),
    out(""),
    *[
        out(name)
        for name in ["a$b", "$a", ".a", "a.", "system.x", "system.views", "x" * 256, "new"]
    ],
    out("c"),
    out({"db": "@self", "coll": "t", "timeseries": {"timeField": "ts"}}),
    out({"db": "@self", "coll": "t", "timeseries": 1}),
)
scenario(
    "$out: what it leaves",
    out("t", target=[]),
    out("new"),
    out("t", before=[{"$match": {"_id": 99}}]),
    out("new", before=[{"$match": {"_id": 99}}]),
    out("t", source=[]),
    out("t", before=[{"$sort": {"a": -1}}]),
    out("t", before=[{"$project": {"_id": 0}}]),
    out("t", before=[{"$project": {"a": 1, "_id": 1}}]),
    out("t", before=[{"$project": {"a": 1, "k": 1, "_id": "$a"}}]),
    out("t", before=[{"$replaceWith": {"z": "$a", "_id": "$_id", "y": 1}}]),
    out("t", before=[{"$project": {"_id": {"$literal": 5}}}]),
    out("t", before=[{"$set": {"_id": [1]}}]),
    out("t", before=[{"$set": {"_id": None}}]),
    out("t", before=[{"$set": {"_id": {"$literal": {"$x": 1}}}}]),
    out("t", before=[{"$replaceWith": {"$literal": {"_id": 1, "a.b": 1, "$c": 2}}}]),
    out("t", before=[{"$group": {"_id": "$k", "n": {"$sum": "$a"}}}, {"$sort": {"_id": -1}}]),
    out("c", before=[{"$match": {"_id": {"$gt": 1}}}]),
    out("c", before=[{"$project": {"_id": {"$literal": 5}}}]),
    out("t", before=[{"$unionWith": "t"}]),
    out(
        "t",
        before=[{"$lookup": {"from": "t", "localField": "_id", "foreignField": "_id", "as": "j"}}],
    ),
)
scenario(
    "$out: the target's indexes and options",
    out("t", setup=[index({"b": 1}), index({"k": -1}, sparse=True)]),
    out("t", setup=[index({"a": 1}, unique=True)], target=[{"_id": 7, "a": 30}]),
    out("t", setup=[index({"k": 1}, unique=True)], before=[{"$set": {"k": "same"}}]),
    out("t", setup=[index({"a": 1}, unique=True)], before=[{"$set": {"a": 1}}], target=[]),
    out("t", target=[], setup=[{"create": "t", "validator": {"a": {"$gt": 15}}}]),
    out(
        "t",
        target=[],
        setup=[{"create": "t", "validator": {"a": {"$gt": 15}}}],
        bypassDocumentValidation=True,
    ),
    out(
        "t",
        target=[],
        setup=[{"create": "t", "validator": {"a": {"$gt": 5}}, "validationLevel": "moderate"}],
    ),
    out("t", target=[], setup=[{"create": "t", "capped": True, "size": 4096}]),
    out("t", target=[], setup=[{"create": "t", "collation": {"locale": "en", "strength": 2}}]),
    out(
        "t",
        target=[],
        setup=[{"create": "t", "collation": {"locale": "en", "strength": 2}}],
        source=[{"_id": "a"}, {"_id": "A"}],
    ),
    out("t", target=[], setup=[{"create": "t", "changeStreamPreAndPostImages": {"enabled": True}}]),
    out(
        "t",
        target=[],
        setup=[
            {
                "create": "t",
                "expireAfterSeconds": 5,
                "clusteredIndex": {"key": {"_id": 1}, "unique": True},
            }
        ],
    ),
    out("v", setup=[{"create": "v", "viewOn": "t", "pipeline": []}]),
    out("t", setup=[index({"b": 1}, expireAfterSeconds=1000), index({"k": "text"}, name="txt")]),
    out("t", setup=[index({"b": 1}, hidden=True), index({"b": 1, "k": 1}, name="bk")]),
)
scenario(
    "$out: where the stage may stand",
    case(SOURCE, TARGET, [{"$out": "t"}, {"$match": {}}]),
    case(SOURCE, TARGET, [{"$out": "t"}, {"$out": "t"}]),
    case(SOURCE, TARGET, [{"$out": "t"}, {"$merge": "t"}]),
    case(SOURCE, TARGET, [{"$facet": {"x": [{"$out": "t"}]}}]),
    case(SOURCE, TARGET, [{"$lookup": {"from": "t", "pipeline": [{"$out": "t"}], "as": "x"}}]),
    case(SOURCE, TARGET, [{"$unionWith": {"coll": "t", "pipeline": [{"$out": "t"}]}}]),
    case(SOURCE, TARGET, [{"$out": "t"}], cursor={"batchSize": 0}),
    case(SOURCE, TARGET, [{"$out": "t"}], writeConcern={"w": 1}),
    case(SOURCE, TARGET, [{"$out": "t"}], readConcern={"level": "majority"}),
    case(SOURCE, TARGET, [{"$out": "t"}], readConcern={"level": "linearizable"}),
    case(SOURCE, TARGET, [{"$out": "t"}], readConcern={"level": "snapshot"}),
    case(SOURCE, TARGET, [{"$out": "t"}], collation={"locale": "en", "strength": 2}),
    case(SOURCE, TARGET, [{"$out": "t"}], explain=True),
    case(SOURCE, TARGET, [{"$merge": "t"}], explain=True),
)

# ------------------------------------------------------------------ running

UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
OID_TEXT = re.compile(r"ObjectId\('[0-9a-f]{24}'\)")
TMP_TEXT = re.compile(r"tmp\.agg_out\.[0-9a-f-]+")
SOON = datetime.timedelta(days=1)
EXPLAIN_KEEP = {"ok", "errmsg", "code", "codeName"}


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
        # An open cursor's id is the server's own choice.
        return "CURSOR" if key == "id" and int(value) != 0 else f"Int64({int(value)})"
    if isinstance(value, float):
        return "NaN" if value != value else f"double({value!r})"
    if isinstance(value, bytes) and key == "uuid":
        return "UUID"
    if isinstance(value, str):
        text = OID_TEXT.sub("OID", value.replace(db, "@db"))
        text = TMP_TEXT.sub("tmp.agg_out.UUID", UUID_TEXT.sub("UUID", text))
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


def here(value, db):
    """`@self` in a command stands for the scenario's own database."""
    if isinstance(value, dict):
        return {k: here(v, db) for k, v in value.items()}
    if isinstance(value, list):
        return [here(v, db) for v in value]
    return db if value == "@self" else value


def held(wire, uri, db, coll):
    """What a collection holds: its documents, its indexes, its options."""
    wire, found = send(wire, uri, db, {"find": coll, "batchSize": 500})
    docs = found.get("cursor", {}).get("firstBatch", found)
    wire, listed = send(wire, uri, db, {"listIndexes": coll})
    indexes = [
        {k: v for k, v in spec.items() if k not in ("v", "ns")}
        for spec in listed.get("cursor", {}).get("firstBatch", [])
    ]
    wire, info = send(wire, uri, db, {"listCollections": 1, "filter": {"name": coll}})
    entry = info.get("cursor", {}).get("firstBatch", [])
    options = [{"type": e.get("type"), "options": e.get("options")} for e in entry]
    return wire, {
        "docs": docs,
        "indexes": sorted(indexes, key=lambda spec: spec.get("name", "")),
        "options": options,
    }


def run_scenario(uri, number, steps):
    db = f"outm{RUN}_{number}"
    wire = Wire(uri)
    replies = []
    for source, target, command, setup in steps:
        for name in ("v", "c", "t", "new"):
            wire, _ = send(wire, uri, db, {"drop": name})
        failed = None
        for prepare in setup:
            wire, made = send(wire, uri, db, prepare)
            if made.get("ok") != 1.0 or made.get("writeErrors"):
                failed = made
        for name, rows in (("c", source), ("t", target)):
            if rows and failed is None:
                wire, made = send(wire, uri, db, {"insert": name, "documents": rows})
                if made.get("n") != len(rows):
                    failed = made
        if failed is not None:
            replies.append({"probe": "SETUP", "reply": clean(failed, db)})
            continue
        wire, reply = send(wire, uri, db, here(command, db))
        if command.get("explain"):
            reply = {k: v for k, v in reply.items() if k in EXPLAIN_KEEP}
        state = {}
        for name in ("c", "t", "new"):
            wire, state[name] = held(wire, uri, db, name)
        replies.append({"reply": clean(reply, db), "after": clean(state, db)})
    wire, _ = send(wire, uri, db, {"dropDatabase": 1})
    wire.close()
    return replies


def parts(want, got):
    """The halves of a case that differ: the reply, or a collection's state."""
    if not ("after" in want and "after" in got):
        return [("", want, got)]
    out_ = []
    if repr(want["reply"]) != repr(got["reply"]):
        out_.append((" reply", want["reply"], got["reply"]))
    for name in want["after"]:
        for facet in ("docs", "indexes", "options"):
            w, g = want["after"][name][facet], got["after"][name][facet]
            if repr(w) != repr(g):
                out_.append((f" {name} {facet}", w, g))
    return out_


def show(step):
    source, target, command, setup = step
    text = f"{command['pipeline']!r}"[:420]
    extra = {k: v for k, v in command.items() if k not in ("aggregate", "pipeline", "cursor")}
    if command.get("cursor"):
        extra["cursor"] = command["cursor"]
    if extra:
        text += f"  with {extra!r}"[:200]
    if source is not SOURCE:
        text += f"  source {source!r}"[:200]
    if target is not TARGET:
        text += f"  target {target!r}"[:200]
    return f"{text}  after {setup!r}"[:900] if setup else text


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "900"))

    # Self-check: the reference must seed, write and read back as the probe assumes.
    check = run_scenario(
        mongod_uri,
        0,
        [
            merge("t"),
            out("t", setup=[index({"b": 1})]),
            merge("new", before=[{"$match": {"_id": 1}}]),
        ],
    )
    wanted = [
        ("t", [2, 9, 1, 3], ["_id_"]),
        ("t", [1, 2, 3], ["_id_", "b_1"]),
        ("new", [1], ["_id_"]),
    ]
    for got, (name, ids, names) in zip(check, wanted, strict=True):
        after = got.get("after", {}).get(name, {})
        ok = got.get("reply", {}).get("ok") == "double(1.0)"
        if (
            not ok
            or [d["_id"] for d in after.get("docs", [])] != ids
            or [i["name"] for i in after.get("indexes", [])] != names
        ):
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
        f"\n=== $out and $merge: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
