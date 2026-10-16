"""Gap tests for Rust MongoDB server items found on 2026-10-16 while
working RM-074 (measured against mongod 8.2.11): the order a FETCH stage
rechecks its predicates in, the wrappers around an error raised inside a
query's `$expr`, and `$abs`.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _db(client: Any, prefix: str) -> Any:
    return client[f"{prefix}_{uuid.uuid4().hex[:8]}"]


def _fetch_filters(client: Any) -> list:
    db = _db(client, "rm175")
    db.c.insert_one({"z": 1})
    db.c.create_index("z")
    out = []
    for f in [
        {"z": 1, "a": {"$gt": 1}, "b": 2},
        {"z": 1, "a": {"$type": "int"}, "b": {"$in": [1, 2]}},
        {"z": 1, "a": {"$gt": 1}, "a.b": 2, "b": {"$lt": 4}},
        {"z": 1, "$nor": [{"a": 1}], "b": 2},
        {"z": 1, "b": 2, "$expr": {"$eq": ["$b", "$a"]}},
    ]:
        plan = db.c.find(f).explain()["queryPlanner"]["winningPlan"]
        out.append((plan["stage"], plan.get("filter")))
    return out


@pytest.mark.backlog_fixed("RM-175")
def test_a_fetch_rechecks_its_predicates_by_path(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 orders the predicates a FETCH stage rechecks by path
    (`$or`, `$not` and `$expr` first), not in `parsedQuery`'s order:
    `{z: 1, a: {$gt: 1}, b: 2}` over an index on `z` rechecks `a` before
    `b`."""
    assert _fetch_filters(rust_mongo) == _fetch_filters(mongod)


def _expr_errors(client: Any) -> list:
    db = _db(client, "rm176")
    db.c.insert_one({"a": 1})
    db.create_collection("e")
    out = []
    for coll in ("c", "e"):
        for f in [{"$expr": {"$divide": ["$a", 0]}}, {"$expr": {"$divide": [1, 0]}}]:
            for run in (
                lambda c=coll, f=f: list(db[c].find(f)),
                lambda c=coll, f=f: db.command("count", c, query=f)["n"],
                lambda c=coll, f=f: db[c].distinct("a", f),
                lambda c=coll, f=f: list(db[c].aggregate([{"$match": f}])),
                lambda c=coll, f=f: db[c].find_one_and_update(f, {"$set": {"q": 1}}),
                lambda c=coll, f=f: db.command("explain", {"find": c, "filter": f})["ok"],
            ):
                out.append(mongo_answer(run))
            for name in ("update", "delete"):
                cmd = (
                    {"update": coll, "updates": [{"q": f, "u": {"$set": {"q": 1}}}]}
                    if name == "update"
                    else {"delete": coll, "deletes": [{"q": f, "limit": 0}]}
                )
                reply = db.command(cmd, check=False)
                out.append([(e["code"], e["errmsg"]) for e in reply.get("writeErrors", [])])
    return [str(a).replace(db.name, "@db") for a in out]


@pytest.mark.backlog_fixed("RM-176")
def test_an_error_inside_a_querys_expr_carries_mongods_wrapper(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reports an error raised inside a query's `$expr` under
    the command's executor wrapper (`Executor error during find command:
    <ns> :: caused by ::`, `Plan executor error during update :: caused by
    ::`, ...), and a constant one as `Failed to optimize expression :: caused
    by ::` -- on an empty collection too. `explain` answers either way."""
    assert _expr_errors(rust_mongo) == _expr_errors(mongod)


def _abs_answers(client: Any) -> list:
    from bson.int64 import Int64

    db = _db(client, "rm177")
    db.c.insert_one({"a": [1, 2], "l": Int64(-(2**63))})
    out = []
    for f in [{"$expr": {"$abs": "$a"}}, {"$expr": {"$abs": "$l"}}]:
        out.append(mongo_answer(lambda f=f: list(db.c.find(f, {"_id": 0}))))
    out.append(
        mongo_answer(lambda: list(db.c.aggregate([{"$project": {"_id": 0, "r": {"$abs": "$l"}}}])))
    )
    return [str(a).replace(db.name, "@db") for a in out]


@pytest.mark.backlog_fixed("RM-177")
def test_abs_of_a_non_number_and_of_the_long_minimum(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `$abs` of an array is 28765 in a query's `$expr` too
    (the Rust server answered "not supported"), and `$abs` of the long
    minimum is 28680 (it answered the double 9.223372036854776e18)."""
    assert _abs_answers(rust_mongo) == _abs_answers(mongod)


def _trim_answers(client: Any) -> list:
    db = _db(client, "rm178")
    db.c.insert_one({"a": "x", "n": 5, "o": {"q": 1}})
    out = []
    for op in ("$trim", "$ltrim", "$rtrim"):
        for spec in ({"input": "$a", "chars": "$n"}, {"input": "$o"}, {"input": 5}):
            pipeline = [{"$project": {"_id": 0, "r": {op: spec}}}]
            out.append(mongo_answer(lambda p=pipeline: list(db.c.aggregate(p))))
    for f in ({"$expr": {"$trim": {}}}, {"$expr": {"$regexMatch": {"input": "$a"}}}):
        out.append(mongo_answer(lambda f=f: list(db.c.aggregate([{"$match": f}]))))
    return [str(a).replace(db.name, "@db") for a in out]


@pytest.mark.backlog_fixed("RM-178")
def test_trim_refuses_a_non_string_by_name(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `$trim` / `$ltrim` / `$rtrim` of a non-string input is
    50699 and of non-string `chars` 50700, each naming the operator and the
    value; the Rust server answered "not supported". A missing required
    argument inside a query's `$expr` stays a bare parse error."""
    assert _trim_answers(rust_mongo) == _trim_answers(mongod)
