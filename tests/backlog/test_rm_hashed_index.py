"""Regression tests for hashed indexes on the Rust MongoDB server, against
mongod 8.2.11 (measured 2026-11-20 and 2026-11-23; RM-732).

A hashed index works on a single-node mongod: it is not a sharding-only
feature. The Rust server refused every one with 67 CannotCreateIndex ("not
supported by the Rust storage engine yet"); it builds them as mongod does,
keyed by the hash of each value, and plans equality and `$in` over them.
"""

from __future__ import annotations

import datetime
import re
import uuid
from typing import Any

import bson
import pytest
from bson.decimal128 import Decimal128

from tests.backlog.conftest import mongo_answer, mongod_rs, rust_mongo  # noqa: F401  (fixtures)


def _created(client: Any, coll: str, spec: dict) -> tuple:
    """`createIndexes` as `("ok",)` or `("err", code, errmsg)`."""
    db = client.rm_hashed
    ans = mongo_answer(lambda: db.command("createIndexes", coll, indexes=[spec]))
    return ("ok",) if ans[0] == "ok" else ans


def _plan_shape(plan: dict) -> list:
    """The winning plan's stages, outermost first, with each index scan's key
    pattern and bounds."""
    out = []
    while plan:
        step = [plan["stage"]]
        if plan["stage"] == "IXSCAN":
            step += [dict(plan["keyPattern"]), dict(plan["indexBounds"])]
        out.append(step)
        plan = plan.get("inputStage")
    return out


def _hashed_basics(client: Any) -> tuple:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    made = _created(client, "c", {"key": {"a": "hashed"}, "name": "a_hashed"})
    listed = [dict(i["key"]) for i in db.c.list_indexes()]
    db.c.insert_many([{"a": i} for i in range(20)])
    eq = _plan_shape(db.c.find({"a": 3}).explain()["queryPlanner"]["winningPlan"])
    rng = _plan_shape(db.c.find({"a": {"$gt": 3}}).explain()["queryPlanner"]["winningPlan"])
    found = list(db.c.find({"a": 3}, {"_id": 0}))
    client.drop_database("rm_hashed")
    return made, listed, eq, rng, found


@pytest.mark.backlog_fixed("RM-732")
def test_a_hashed_index_is_built_and_serves_equality(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11: `{a: "hashed"}` is created and listed; `{a: 3}` is a
    FETCH over an IXSCAN of it with the point bounds
    `[-1529429926180645992, -1529429926180645992]` (mongod's hash of 3); a
    range query cannot use it and is a COLLSCAN."""
    theirs = _hashed_basics(mongod_rs)
    assert theirs[0] == ("ok",), theirs
    assert _hashed_basics(rust_mongo) == theirs


def _refusals(client: Any) -> list:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    out = [
        _created(client, "u", {"key": {"b": "hashed"}, "name": "b_h", "unique": True}),
        _created(client, "two", {"key": {"x": "hashed", "y": "hashed"}, "name": "xy"}),
    ]
    _created(client, "arr", {"key": {"a": "hashed"}, "name": "a_hashed"})
    reply = db.command("insert", "arr", documents=[{"a": [1, 2]}])
    out.append([(e["code"], e["errmsg"]) for e in reply.get("writeErrors", [])])
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-732")
def test_hashed_index_refusals_are_mongods(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a unique hashed index with 16764 ("Currently
    hashed indexes cannot guarantee uniqueness. Use a regular index."), two
    hashed fields with 31303 ("A maximum of one index field is allowed to be
    hashed ..."), and an array value under a hashed index with the write
    error 16766 ("Error: hashed indexes do not currently support array
    values. Found array at path: a")."""
    theirs = _refusals(mongod_rs)
    assert [r[1] for r in theirs[:2]] == [16764, 31303], theirs
    assert [c for c, _ in theirs[2]] == [16766], theirs
    assert _refusals(rust_mongo) == theirs


@pytest.mark.backlog_fixed("RM-732")
def test_a_compound_index_with_one_hashed_field_is_built(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 accepts `{x: 1, y: "hashed"}`: one hashed field may sit
    in a compound index."""

    def made(client: Any) -> tuple:
        client.drop_database("rm_hashed")
        ans = _created(client, "cmp", {"key": {"x": 1, "y": "hashed"}, "name": "x1_yh"})
        client.drop_database("rm_hashed")
        return ans

    theirs = made(mongod_rs)
    assert theirs == ("ok",), theirs
    assert made(rust_mongo) == theirs


# ---------------------------------------------------------------- RM-732 --
# What the item's fix pins beyond its three gap tests, each against mongod
# 8.2.11 (measured 2026-11-23).


def _stages(plan: dict) -> list:
    """`_plan_shape` with each plan's filter, and an OR's / AND's branches."""
    out = []
    while plan:
        step: list = [plan["stage"]]
        if plan["stage"] == "IXSCAN":
            step += [dict(plan["keyPattern"]), dict(plan["indexBounds"]), plan["direction"]]
            step.append("multiKeyPaths" in plan)
        if "filter" in plan:
            step.append(repr(plan["filter"]))
        if "inputStages" in plan:
            step.append([_stages(i) for i in plan["inputStages"]])
        out.append(step)
        plan = plan.get("inputStage")
    return out


def _explained(db: Any, coll: str, query: dict, sort: Any = None, hint: Any = None) -> Any:
    cur = db[coll].find(query)
    if sort:
        cur = cur.sort(sort)
    if hint:
        cur = cur.hint(hint)
    plans = cur.explain()["queryPlanner"]
    return _stages(plans["winningPlan"]), [_stages(p) for p in plans["rejectedPlans"]]


def _plans(client: Any) -> dict:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    db.p.insert_many([{"_id": i, "a": i, "b": i % 3, "d": {"x": i}} for i in range(30)])
    db.p.create_index([("a", "hashed")], name="ah")
    db.q.insert_many([{"_id": i, "a": i, "b": i % 3} for i in range(30)])
    for key, name in [
        ([("a", "hashed")], "ah"),
        ([("a", 1)], "a1"),
        ([("b", 1)], "b1"),
        ([("b", 1), ("a", "hashed")], "bah"),
    ]:
        db.q.create_index(key, name=name)
    db.c.insert_many([{"_id": i, "x": i % 4, "y": i, "z": i % 5} for i in range(40)])
    db.c.create_index([("x", 1), ("y", "hashed")], name="xy")
    db.c.create_index([("y", "hashed"), ("z", -1)], name="yz")
    out: dict = {}
    for n, q in {
        "eq": {"a": 3},
        "eq_op": {"a": {"$eq": 3.5}},
        "in": {"a": {"$in": [3, 4, 5, 3, 3.0]}},
        "in_one": {"a": {"$in": [3]}},
        "in_none": {"a": {"$in": []}},
        "in_array": {"a": {"$in": [[1], 3, None]}},
        "range": {"a": {"$gt": 3}},
        "ne": {"a": {"$ne": 3}},
        "exists": {"a": {"$exists": True}},
        "missing": {"a": {"$exists": False}},
        "null": {"a": None},
        "doc": {"a": {"x": 1}},
        "array": {"a": [1, 2]},
        "empty_array": {"a": []},
        "regex": {"a": {"$regex": "x"}},
        "or": {"$or": [{"a": 1}, {"a": 2}]},
        "or_other": {"$or": [{"a": 1}, {"b": 2}]},
        "and_in_eq": {"$and": [{"a": {"$in": [1, 2, 3]}}, {"a": 2}]},
        "other_field": {"b": 1},
        "date": {"a": datetime.datetime(2020, 1, 1)},
        "minkey": {"a": bson.MinKey()},
    }.items():
        out[n] = _explained(db, "p", q)
    out["sort_eq"] = _explained(db, "p", {"a": 3}, sort=[("a", 1)])
    out["sort_other"] = _explained(db, "p", {"a": 3}, sort=[("b", 1)])
    out["hint_sort"] = _explained(db, "p", {}, sort=[("a", -1)], hint="ah")
    out["hint_range"] = _explained(db, "p", {"a": {"$gt": 3}}, hint="ah")
    out["hint_pattern"] = _explained(db, "p", {"b": 1}, hint=[("a", "hashed")])
    for n, q in {
        "eq": {"a": 3},
        "in": {"a": {"$in": [1, 2]}},
        "eq_b": {"a": 3, "b": 0},
        "range": {"a": {"$gt": 25}},
    }.items():
        out["multi " + n] = _explained(db, "q", q)
    for n, q in {
        "x": {"x": 1},
        "x_y": {"x": 1, "y": 5},
        "y": {"y": 5},
        "x_in_y": {"x": {"$in": [1, 2]}, "y": 5},
        "x_y_in": {"x": 1, "y": {"$in": [1, 5]}},
        "x_y_gt": {"x": 1, "y": {"$gt": 5}},
        "y_z": {"y": 5, "z": 0},
        "z": {"z": 1},
    }.items():
        out["compound " + n] = _explained(db, "c", q)
    out["compound sort_x"] = _explained(db, "c", {}, sort=[("x", 1)], hint="xy")
    out["compound sort_x_y"] = _explained(db, "c", {"x": 1}, sort=[("y", 1)])
    out["compound sort_x_desc"] = _explained(
        db, "c", {"x": {"$gt": 1}}, sort=[("x", -1)], hint="xy"
    )
    for cmd in (
        {"count": "p", "query": {"a": 3}},
        {"distinct": "p", "key": "b", "query": {"a": 3}},
        {"update": "p", "updates": [{"q": {"a": 3}, "u": {"$set": {"b": 1}}}]},
        {"delete": "p", "deletes": [{"q": {"a": 3}, "limit": 0}]},
        {"aggregate": "p", "pipeline": [{"$match": {"a": 3}}], "cursor": {}},
    ):
        plan = db.command({"explain": cmd, "verbosity": "queryPlanner"})["queryPlanner"]
        out["explain " + next(iter(cmd))] = _stages(plan["winningPlan"])
    stats = db.command(
        {
            "explain": {"find": "p", "filter": {"a": {"$in": [3, 4, 99]}}},
            "verbosity": "executionStats",
        }
    )["executionStats"]
    out["stats in"] = {k: stats[k] for k in ("nReturned", "totalKeysExamined", "totalDocsExamined")}
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-732")
def test_hashed_index_plans_are_mongods(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11: an equality, an `$in`, a null or `$exists: false` reads
    a hashed index at the hashes of its values (an array operand bounds as
    on a plain index, by its first element and the whole array); `$exists:
    true` and a hinted scan read the whole index; no range, negation, regex
    or type uses it; it serves no sort except where an equality pins the
    field; it is intersected with another index's scan; it reports no
    `multiKeyPaths`; and `count`, `distinct`, `update`, `delete` and
    `aggregate` plan as `find` does."""
    theirs = _plans(mongod_rs)
    assert theirs["eq"][0][-1][1] == {"a": "hashed"}, theirs["eq"]
    assert _plans(rust_mongo) == theirs


def _every_type() -> list:
    return [
        1,
        2**31,
        2**62,
        -(2**63),
        0.5,
        -0.0,
        float("nan"),
        float("inf"),
        float("-inf"),
        1.5e300,
        Decimal128("1.5"),
        Decimal128("NaN"),
        Decimal128("12345678901234567890"),
        "",
        "a",
        "\u00e9",
        "e\u0301",
        "x" * 200,
        {},
        {"a": 1},
        {"a": 1.0},
        {"a": {"b": [1, 2]}},
        {"b": 1, "a": 2},
        {"": 1},
        None,
        True,
        False,
        datetime.datetime(2020, 1, 1),
        datetime.datetime(1960, 1, 1, 12, 30, 45, 123000),
        bson.ObjectId("507f1f77bcf86cd799439011"),
        bson.Binary(b"abc"),
        bson.Binary(b"abc", 5),
        bson.Binary(b"abc", 128),
        bson.Binary(uuid.UUID(int=5).bytes, 4),
        bson.Regex("a.*b", "i"),
        bson.Timestamp(5, 6),
        bson.Code("x=1"),
        bson.Code("x=1", {"y": 2}),
        bson.MinKey(),
        bson.MaxKey(),
        bson.Int64(2**40),
    ]


def _typed(client: Any) -> dict:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    values = _every_type()
    db.t.insert_many([{"_id": i, "a": v} for i, v in enumerate(values)] + [{"_id": 1000}])
    db.t.create_index([("a", "hashed")], name="ah")
    out: dict = {"scan": [d["_id"] for d in db.t.find().hint("ah")]}
    for i, v in enumerate(values):
        plan = db.t.find({"a": v}).explain()["queryPlanner"]["winningPlan"]
        out[i] = (_stages(plan), [d["_id"] for d in db.t.find({"a": v})])
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-732")
def test_hashed_index_keys_every_bson_type_as_mongod_does(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11: the keys of a hashed index are the 64-bit hashes of the
    values of every BSON type, so a scan of the index meets the documents in
    mongod's order (a missing field hashes as null), and an equality bounds
    at mongod's hash."""
    theirs = _typed(mongod_rs)
    assert len(theirs["scan"]) == len(_every_type()) + 1
    assert _typed(rust_mongo) == theirs


def _collated(client: Any) -> list:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    db.co.insert_many([{"_id": 1, "a": "abc"}, {"_id": 2, "a": "ABC"}, {"_id": 3, "a": "abd"}])
    out = [
        _created(
            client,
            "co",
            {"key": {"a": "hashed"}, "name": "ah", "collation": {"locale": "en", "strength": 2}},
        )
    ]
    for coll in (
        None,
        {"locale": "en", "strength": 2},
        {"locale": "en", "strength": 3},
        {"locale": "simple"},
    ):
        cur = db.co.find({"a": "abc"})
        if coll:
            cur = cur.collation(coll)
        out.append(_stages(cur.explain()["queryPlanner"]["winningPlan"]))
        out.append(sorted(d["_id"] for d in cur))
    out.append([d["_id"] for d in db.co.find().hint("ah")])
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-732")
def test_a_hashed_index_with_a_collation_hashes_collation_keys(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 hashes a string's collation key under an index's
    collation, so `"abc"` and `"ABC"` share a key at strength 2, and a query
    under that collation (only) reads the index."""
    theirs = _collated(mongod_rs)
    assert theirs[3][0][0] == "FETCH", theirs
    assert _collated(rust_mongo) == theirs


_ARRAY_REFUSAL = re.compile(
    r"hashed indexes do not currently support array values. Found array at path: (\S+)"
)


def _array_writes(client: Any) -> list:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    db.w.create_index([("a", "hashed")], name="ah")
    db.w.create_index([("x", 1), ("y", "hashed")], name="xy")
    db.w.insert_one({"_id": "u", "a": 1, "x": 1, "y": 1})

    def refused(fn: Any) -> Any:
        ans = mongo_answer(fn)
        if ans[0] == "ok":
            return ("ok",)
        found = _ARRAY_REFUSAL.search(ans[2])
        return (
            ans[0],
            ans[1],
            ans[2].split(" :: caused by :: ")[0] if found else ans[2],
            found and found[1],
        )

    out = [
        refused(lambda: db.w.update_one({"_id": "u"}, {"$set": {"a": [1, 2]}})),
        refused(lambda: db.w.update_one({"_id": "u"}, {"$set": {"x": [1]}})),
        refused(lambda: db.w.update_one({"_id": "u"}, {"$set": {"y": [1]}})),
        refused(lambda: db.w.update_one({"_id": "u"}, {"$push": {"y": 1}})),
        refused(lambda: db.w.replace_one({"_id": "u"}, {"a": [3]})),
        refused(lambda: db.w.find_one_and_update({"_id": "u"}, {"$set": {"a": [3]}})),
        refused(lambda: db.w.update_one({"_id": "new"}, {"$set": {"a": [1]}}, upsert=True)),
        refused(lambda: db.w.insert_one({"a": {"b": [1]}})),
        refused(lambda: db.w.insert_one({"x": [1, 2], "y": 2})),
        refused(lambda: db.w.insert_one({"a": []})),
        refused(lambda: db.w.update_one({"_id": "u"}, {"$unset": {"a": 1}})),
        refused(lambda: db.w.update_one({"_id": "u"}, {"$set": {"a": {"b": [1]}}})),
    ]
    out.append(sorted(str(d) for d in db.w.find({}, {"_id": 0, "a": 1, "x": 1, "y": 1})))
    db.b.insert_many([{"a": 1}, {"a": [1, 2]}, {"a": 3}])
    ans = mongo_answer(lambda: db.b.create_index([("a", "hashed")], name="ah"))
    out.append((ans[0], ans[1], bool(_ARRAY_REFUSAL.search(ans[2]))))
    out.append([i["name"] for i in db.b.list_indexes()])
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-732")
def test_a_hashed_index_refuses_an_array_wherever_it_is_written(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11: an array at a hashed index's field, or at the plain
    field of a compound hashed index, is refused with 16766 by an insert,
    an update (the pipeline's `$set` and `$push` included), a replacement, a
    `findAndModify`, an upsert and an index build; an empty array is an
    array, a path through a subdocument names the array's own path, and the
    document stays as it was."""
    theirs = _array_writes(mongod_rs)
    assert theirs[0][1] == 16766, theirs
    assert _array_writes(rust_mongo) == theirs


def _specs(client: Any) -> dict:
    out: dict = {}
    for key, opts in [
        ({"a": "hashed", "b": "hashed"}, {"unique": True}),
        ({"a": "hashed"}, {"unique": False}),
        ({"a": "hashed"}, {"unique": 1}),
        ({"a": "hashed"}, {"unique": 0}),
        ({"a": "hashed", "$b": "hashed"}, {}),
        ({"a": "hashed", "b": "hashed", "c": "hashed"}, {}),
        ({"a": "hashed", "b": 1, "c": "hashed"}, {}),
        ({"a": "hashed", "b": 1, "c": 1}, {}),
        ({"b": -1, "a": "hashed"}, {}),
        ({"a": "hashed", "b": "2dsphere"}, {}),
        ({"a": "hashed", "b": "text"}, {}),
        ({"a": "hashed", "b": "2d"}, {}),
        ({"a": "hashed", "$**": 1}, {}),
        ({"$**": "hashed"}, {}),
        ({"a": "HASHED"}, {}),
        (
            {
                "a": "hashed ",
            },
            {},
        ),
        ({"a": "hashed"}, {"bogus": 1}),
        ({"a": "hashed"}, {"sparse": True}),
        ({"a": "hashed"}, {"hidden": True}),
        ({"a": "hashed"}, {"expireAfterSeconds": 10}),
        ({"a": "hashed"}, {"partialFilterExpression": {"b": {"$gt": 1}}}),
        ({"a": "hashed"}, {"collation": {"locale": "en", "strength": 2}}),
        ({"a.b": "hashed"}, {}),
        ({"_id": "hashed"}, {}),
    ]:
        client.drop_database("rm_hashed")
        out[f"{key} {opts}"] = _created(client, "s", {"key": key, "name": "i1", **opts})
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    db.n.create_index([("a", "hashed"), ("b", -1)])
    db.n.create_index([("c", "hashed")])
    out["names"] = [i["name"] for i in db.n.list_indexes()]
    out["listed"] = [dict(i) for i in db.n.list_indexes()]
    db.d.create_index([("a", 1)], name="a1")
    out["same key, another kind"] = [
        _created(client, "d", {"key": {"a": "hashed"}, "name": "ah"}),
        _created(client, "d", {"key": {"a": "hashed"}, "name": "ah2"}),
        mongo_answer(lambda: db.d.drop_index([("a", "hashed")]))[0],
        mongo_answer(lambda: db.d.drop_index([("a", "hashed")]))[:2],
    ]
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-732")
def test_hashed_index_specs_are_validated_as_mongod_validates_them(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11: the code and message of every refusal of a hashed
    index spec, the options that are accepted with one, a hashed `_id`
    index, the name a driver gives it, and its place beside a plain index of
    the same field."""
    theirs = _specs(mongod_rs)
    assert theirs["{'a': 'hashed', 'b': 'hashed'} {'unique': True}"][1] == 16764
    assert _specs(rust_mongo) == theirs


def _time_series(client: Any) -> list:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    db.command("create", "ts", timeseries={"timeField": "t", "metaField": "m"})
    out = []
    for i, key in enumerate(
        [
            {"m": "hashed"},
            {"m.x": "hashed"},
            {"m": "hashed", "t": 1},
            {"t": "hashed"},
            {"x": "hashed"},
            {"m": 1, "x": "hashed"},
        ]
    ):
        out.append(_created(client, "ts", {"key": key, "name": f"n{i}"}))
    out.append([(i["name"], dict(i["key"])) for i in db.ts.list_indexes()])
    out.append([(i["name"], dict(i["key"])) for i in db["system.buckets.ts"].list_indexes()])
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-732")
def test_a_time_series_collection_takes_a_hashed_index_on_its_meta_field(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 builds a hashed index on the `metaField` (and below it)
    of a time-series collection, on its buckets as `meta`; on the time field
    or a measurement it refuses one with 67."""
    theirs = _time_series(mongod_rs)
    assert theirs[0] == ("ok",), theirs
    assert _time_series(rust_mongo) == theirs
