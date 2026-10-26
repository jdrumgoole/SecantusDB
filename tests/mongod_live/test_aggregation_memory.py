"""Aggregation memory budgets against mongod 8.2 (RM-211).

mongod holds each blocking stage to a budget it can lower at runtime with
`setParameter`, so every case here lowers the budget on both servers rather
than loading 100 MB: a `$group` that spills (the same groups, in key order)
or, with `allowDiskUse: false`, refuses (292); one `$push` past its budget
(146) in each engine's words; a `$setWindowFields` partition (5643011) and a
`$facet` document (4031700) past theirs, byte counts included; and the
knobs themselves, as `getParameter` / `setParameter` answer them. The
100 MB cases are the backlog's regression test,
`tests/backlog/test_rm_found_2026_10_20b.py::test_aggregation_memory_limits`.
"""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any

import bson
from pymongo.errors import PyMongoError

from tests.mongod_live.conftest import mongo_answer, same

DEFAULT = 104_857_600

#: 400 documents in 23 groups, numbers of each type and a 100-byte string.
DOCS = [
    {
        "_id": i,
        "k": (i * 7) % 23,
        "v": i / 4,
        "n": bson.Int64(i),
        "s": f"{i:04}" * 25,
    }
    for i in range(400)
]

GROUP = [
    {
        "$group": {
            "_id": "$k",
            "sum": {"$sum": "$v"},
            "ints": {"$sum": "$_id"},
            "longs": {"$sum": "$n"},
            "avg": {"$avg": "$n"},
            "n": {"$count": {}},
            "min": {"$min": "$s"},
            "max": {"$max": "$v"},
            "first": {"$first": "$_id"},
            "last": {"$last": "$_id"},
            "pushed": {"$push": "$_id"},
            "set": {"$addToSet": "$k"},
        }
    }
]


@contextmanager
def knob(client: Any, name: str, value: int) -> Iterator[None]:
    client.admin.command({"setParameter": 1, name: value})
    try:
        yield
    finally:
        client.admin.command({"setParameter": 1, name: DEFAULT})


def _load(client: Any) -> Any:
    client.drop_database("rm211live")
    coll = client.rm211live.c
    coll.insert_many(DOCS)
    return coll


def _run(coll: Any, pipeline: list, **options: Any) -> tuple:
    return mongo_answer(lambda: list(coll.aggregate(pipeline, **options)))


def _group(client: Any, allow: bool | None, unwind: bool, budget: int = 10_000) -> tuple:
    coll = _load(client)
    # An `$unwind` first keeps the `$group` out of SBE: the classic engine,
    # which `internalDocumentSourceGroupMaxMemoryBytes` holds.
    pipeline = ([{"$unwind": "$s"}] if unwind else []) + GROUP
    options = {} if allow is None else {"allowDiskUse": allow}
    with knob(client, "internalDocumentSourceGroupMaxMemoryBytes", budget):
        return _run(coll, pipeline, **options)


def test_a_spilled_group_answers_the_same_in_key_order(rust_mongo: Any, mongod: Any) -> None:
    theirs = _group(mongod, None, True)
    assert theirs[0] == "ok", theirs
    assert [d["_id"] for d in theirs[1]] == list(range(23))  # not vacuous: spilled
    same(_group(rust_mongo, None, True), theirs)


def test_a_group_over_budget_without_disk_is_refused(rust_mongo: Any, mongod: Any) -> None:
    theirs = _group(mongod, False, True)
    assert theirs[:2] == ("err", 292), theirs
    assert _group(rust_mongo, False, True) == theirs


def test_an_sbe_group_is_not_held_to_the_classic_budget(rust_mongo: Any, mongod: Any) -> None:
    # SBE's hash aggregation has a budget of its own, which 8.2.11 does not
    # let a client change: the classic knob at 100 bytes leaves it running.
    def answer(client: Any) -> tuple:
        got = _group(client, False, False, budget=100)
        return got if got[0] != "ok" else ("ok", sorted(got[1], key=lambda d: d["_id"]))

    theirs = answer(mongod)
    assert theirs[0] == "ok", theirs
    same(answer(rust_mongo), theirs)


def _push(client: Any, unwind: bool) -> tuple:
    coll = _load(client)
    pipeline = ([{"$unwind": "$s"}] if unwind else []) + [
        {"$group": {"_id": None, "p": {"$push": "$s"}}}
    ]
    with knob(client, "internalQueryMaxPushBytes", 5_000):
        return _run(coll, pipeline)


def test_one_array_past_its_budget_is_refused(rust_mongo: Any, mongod: Any) -> None:
    for unwind in (False, True):
        theirs = _push(mongod, unwind)
        assert theirs[:2] == ("err", 146), theirs
        assert _push(rust_mongo, unwind) == theirs


def _window(client: Any, project: bool) -> tuple:
    coll = _load(client)
    pipeline: list = [{"$setWindowFields": {"sortBy": {"_id": 1}, "output": {"t": {"$sum": "$v"}}}}]
    if project:
        pipeline.append({"$project": {"_id": 1, "t": 1}})
    with knob(client, "internalDocumentSourceSetWindowFieldsMaxMemoryBytes", 20_000):
        return _run(coll, pipeline, allowDiskUse=False)


def test_a_window_partition_over_budget_without_disk_is_refused(
    rust_mongo: Any, mongod: Any
) -> None:
    # Whole documents are cached when the stage's output is the answer;
    # only the fields read are when a `$project` follows.
    for project in (False, True):
        theirs = _window(mongod, project)
        assert theirs[:2] == ("err", 5643011), theirs
        assert _window(rust_mongo, project) == theirs


def _facet(client: Any) -> tuple:
    coll = _load(client)
    with knob(client, "internalQueryFacetMaxOutputDocSizeBytes", 10_000):
        return _run(coll, [{"$facet": {"a": [{"$match": {}}]}}])


def test_a_facet_document_over_budget_is_refused(rust_mongo: Any, mongod: Any) -> None:
    theirs = _facet(mongod)
    assert theirs[:2] == ("err", 4031700), theirs
    assert _facet(rust_mongo) == theirs


KNOBS = [
    "internalDocumentSourceGroupMaxMemoryBytes",
    "internalQueryMaxPushBytes",
    "internalQueryMaxAddToSetBytes",
    "internalQueryMaxConcatArraysBytes",
    "internalQueryMaxSetUnionBytes",
    "internalDocumentSourceSetWindowFieldsMaxMemoryBytes",
    "internalLookupStageIntermediateDocumentMaxSizeBytes",
    "internalQueryFacetMaxOutputDocSizeBytes",
]


def _knobs(client: Any) -> list:
    out = []
    for name in KNOBS:
        out.append(client.admin.command({"getParameter": 1, name: 1})[name])
        for value in (0, 2.7, "x", bson.Int64(2**40), 16_793_600):
            try:
                client.admin.command({"setParameter": 1, name: value})
                out.append(("ok", client.admin.command({"getParameter": 1, name: 1})[name]))
            except PyMongoError as e:
                out.append(("err", getattr(e, "code", None), str(e).split(", full error")[0]))
        client.admin.command({"setParameter": 1, name: DEFAULT})
    return out


def test_the_budgets_are_parameters(rust_mongo: Any, mongod: Any) -> None:
    same(_knobs(rust_mongo), _knobs(mongod))
