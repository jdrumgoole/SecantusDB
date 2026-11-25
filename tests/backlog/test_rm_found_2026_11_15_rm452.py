"""Gap tests for what working RM-452 (the rewrite of a `$match` after a
`$changeStream` into the oplog filter) left on 2026-11-15, comparing with
mongod 8.2.11 as a one-member replica set.

RM-452 ported mongod's rewrite of `operationType`, RM-698's first slice
those of `documentKey` and `fullDocument`, its second that of `ns` by
`$eq` / `$in`, its third that of `to`, its fourth a regex on either,
its fifth a regex in an `$in`, its sixth an `$expr` on `operationType`,
its seventh one on `documentKey`, `ns` or `to`; the rewrites of the other
event fields, and of `$expr` on them, are still
to come; the Boolean simplifier mongod runs
over the unwind's filter came with RM-699 (2026-11-24).
"""

from __future__ import annotations

import re
import uuid
from typing import Any

import pytest


def _explain(client: Any, match: dict, name: str | None = None) -> dict:
    db = client["rm452"]
    name = name or "c" + uuid.uuid4().hex[:8]
    db[name].insert_one({"a": 1})
    pipeline = [{"$changeStream": {}}, {"$match": match}]
    return db.command("aggregate", name, pipeline=pipeline, explain=True)


def _query_hash(client: Any, match: dict) -> str:
    return _explain(client, match)["stages"][0]["$cursor"]["queryPlanner"]["queryHash"]


def _read_and_unwind(client: Any, match: dict) -> tuple:
    db = client["rm452"]
    db["cfixed"].insert_one({"a": 1})
    pipeline = [{"$changeStream": {}}, {"$match": match}]
    stages = db.command("aggregate", "cfixed", pipeline=pipeline, explain=True)["stages"]
    return (
        stages[0]["$cursor"]["queryPlanner"]["queryHash"],
        stages[1]["$changeStream"].get("filter"),
    )


@pytest.mark.backlog_fixed("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"fullDocument.a": 1},
        {"fullDocument.a": None},
        {"fullDocument.a": {"$gt": 3}},
        {"fullDocument.a": {"$in": [1, 2]}},
        {"fullDocument.a": {"$ne": 3}},
        {"fullDocument": {"$exists": False}},
        {"documentKey._id": 1},
        {"documentKey._id": None},
        {"documentKey._id": {"$lte": 3}},
        {"documentKey._id": {"$ne": 3}},
        {"documentKey": {"$exists": True}},
        {"$or": [{"documentKey._id": 1}, {"operationType": "drop"}]},
    ],
)
def test_a_match_on_the_document_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites a `$match` on `documentKey` (`o2` of an update
    or insert, `o` of a delete) and, inexactly, on `fullDocument` (`o` of an
    insert or replacement, every other update) into the oplog filter and the
    unwind's; this server left them out, reading with hash 90A52575."""
    assert _read_and_unwind(rust_mongo, match) == _read_and_unwind(mongod_rs, match)


@pytest.mark.backlog_fixed("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"ns.coll": "c"},
        {"ns.coll": "a.b"},
        {"ns.db": "rm452"},
        {"ns": {"db": "rm452", "coll": "c"}},
        {"ns": {"db": "rm452"}},
        {"ns": {"coll": "c", "db": "rm452"}},
        {"ns": "x"},
        {"ns.coll": {"$in": ["c", "d"]}},
        {"ns.coll": {"$in": ["c", "c"]}},
        {"ns.coll": {"$in": ["c", 1]}},
        {"ns.db": {"$in": ["x", "y"]}},
        {"ns": {"$in": [{"db": "a", "coll": "b"}, {"db": "c"}]}},
        {"ns.coll": {"$ne": "c"}},
        {"$nor": [{"ns": {"db": "rm452"}}]},
        {"ns.coll": 1},
        {"ns.x": "a"},
        {"ns.db": "rm452", "ns.coll": "c"},
        {"$or": [{"ns.coll": "c"}, {"operationType": "drop"}]},
    ],
)
def test_a_match_on_the_namespace_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites an `$eq` / `$in` on `ns`, `ns.db` or `ns.coll`
    into the commands' `o` fields that name it and a write's `ns`
    (`matchRewriteGenericNamespace`); this server left them out, reading
    with hash 90A52575 where mongod's `{"ns.coll": "c"}` is D801EB4B."""
    assert _read_and_unwind(rust_mongo, match) == _read_and_unwind(mongod_rs, match)


@pytest.mark.backlog_fixed("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"to.coll": "c"},
        {"to.coll": "a.b"},
        {"to.db": "rm452"},
        {"to": {"db": "rm452", "coll": "c"}},
        {"to": {"db": "rm452"}},
        {"to": {"coll": "c", "db": "rm452"}},
        {"to": "x"},
        {"to.coll": {"$in": ["c", "d"]}},
        {"to.coll": {"$in": ["c", 1]}},
        {"to.db": {"$in": ["x", "y"]}},
        {"to": {"$in": [{"db": "a", "coll": "b"}, {"db": "c", "coll": "d"}]}},
        {"to": {"$in": [{"db": "a", "coll": "b"}, {"db": "c"}]}},
        {"to.coll": {"$ne": "c"}},
        {"to.coll": 1},
        {"to.x": "a"},
        {"$or": [{"to.coll": "c"}, {"operationType": "insert"}]},
    ],
)
def test_a_match_on_the_rename_target_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites an `$eq` / `$in` on `to`, `to.db` or `to.coll`
    into a predicate on a rename's `o.to` (`matchRewriteGenericNamespace`);
    this server left them out of the read (90A52575 where mongod's
    `{"to.coll": "c"}` is A491FA53)."""
    assert _read_and_unwind(rust_mongo, match) == _read_and_unwind(mongod_rs, match)


@pytest.mark.backlog_fixed("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"ns.coll": {"$regex": "^c"}},
        {"ns.coll": {"$regex": "^C", "$options": "i"}},
        {"ns.coll": re.compile("^c")},
        {"ns.db": {"$regex": "^rm"}},
        {"ns.coll": {"$not": re.compile("^c")}},
        {"ns": {"$regex": "^c"}},
        {"ns.x": {"$regex": "^c"}},
        {"to.coll": {"$regex": "^c"}},
        {"to.db": {"$regex": "x"}},
        {"to": {"$regex": "^c"}},
        {"$or": [{"ns.coll": {"$regex": "^c"}}, {"operationType": "drop"}]},
        {"ns.coll": {"$in": [re.compile("^c")]}},
        {"ns.coll": {"$in": ["x", re.compile("^c", re.I)]}},
        {"ns.coll": {"$in": ["x", "y", re.compile("^c"), re.compile("d$")]}},
        {"ns.db": {"$in": ["x", re.compile("^r")]}},
        {"ns.db": {"$in": [re.compile("^r"), re.compile("x")]}},
        {"ns": {"$in": [re.compile("^c")]}},
        {"ns": {"$in": [{"db": "rm452", "coll": "c"}, re.compile("^c")]}},
        {"ns.x": {"$in": [None, re.compile("^c")]}},
        {"to.coll": {"$in": ["x", re.compile("^c")]}},
        {"to.db": {"$in": [re.compile("^r"), re.compile("x")]}},
        {"to": {"$in": [{"db": "a", "coll": "b"}, re.compile("^c")]}},
        {"to": {"$in": [re.compile("^c")]}},
    ],
)
def test_a_regex_on_the_namespace_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites a regex on `ns.db` / `ns.coll` / `to.db` /
    `to.coll` into an `$expr` of `$regexMatch` over the oplog field's part
    (`matchRewriteGenericNamespace`; hash EE5C1E5A for `ns.coll: /^c/`), and
    a regex on a whole `ns` or `to` into `$alwaysFalse`, and an `$in` holding
    regexes into the `$or` of its equalities' rewrite and each regex's;
    this server left them out of the read (90A52575)."""
    assert _read_and_unwind(rust_mongo, match) == _read_and_unwind(mongod_rs, match)


@pytest.mark.backlog_fixed("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"$expr": {"$eq": ["$operationType", "insert"]}},
        {"$expr": {"$eq": ["insert", "$operationType"]}},
        {"$expr": {"$eq": ["$operationType", "update"]}},
        {"$expr": {"$eq": ["$operationType", "drop"]}},
        {"$expr": {"$eq": ["$operationType", "bogus"]}},
        {"$expr": {"$eq": ["$operationType", 1]}},
        {"$expr": {"$eq": ["$operationType", {"$literal": "insert"}]}},
        {"$expr": {"$ne": ["$operationType", "insert"]}},
        {"$expr": {"$in": ["$operationType", ["insert", "delete"]]}},
        {"$expr": {"$gt": ["$operationType", "a"]}},
        {"$expr": {"$and": [{"$eq": ["$operationType", "insert"]}, {"$eq": ["$x", 1]}]}},
        {
            "$expr": {
                "$and": [{"$eq": ["$operationType", "insert"]}, {"$ne": ["$operationType", "x"]}]
            }
        },
        {
            "$expr": {
                "$or": [
                    {"$eq": ["$operationType", "insert"]},
                    {"$eq": ["$operationType", "delete"]},
                ]
            }
        },
        {"$expr": {"$or": [{"$eq": ["$operationType", "insert"]}, {"$eq": ["$x", 1]}]}},
        {"$expr": {"$not": [{"$eq": ["$operationType", "insert"]}]}},
        {"$or": [{"$expr": {"$eq": ["$operationType", "insert"]}}, {"operationType": "drop"}]},
        {"operationType": "insert", "$expr": {"$ne": ["$operationType", "delete"]}},
        {"$expr": {"$eq": ["$x", 1]}},
    ],
)
def test_an_expr_on_the_operation_type_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites an `$expr` on `$operationType` into an `$expr`
    over the oplog entry (`$operationType` as a `$switch` on `op` and the
    command's field), and the `$_internalExprEq` its optimizer derives from
    an `$eq` as the `operationType` equality's rewrite (hash 6F8558DD for
    `$eq` of `insert`); this server left both out of the read (90A52575)."""
    assert _read_and_unwind(rust_mongo, match) == _read_and_unwind(mongod_rs, match)


@pytest.mark.backlog_fixed("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"$expr": {"$eq": ["$documentKey._id", 1]}},
        {"$expr": {"$eq": ["$documentKey", {"_id": 1}]}},
        {"$expr": {"$eq": ["$documentKey._id", None]}},
        {"$expr": {"$eq": ["$documentKey._id.x", 1]}},
        {"$expr": {"$eq": ["$documentKey._id", [1]]}},
        {"$expr": {"$gt": ["$documentKey._id", 1]}},
        {"$expr": {"$lte": ["$documentKey._id", 1]}},
        {"$expr": {"$in": ["$documentKey._id", [1, 2]]}},
        {"$expr": {"$eq": ["$ns.coll", "c"]}},
        {"$expr": {"$eq": ["$ns.db", "rm452"]}},
        {"$expr": {"$eq": ["$ns", {"db": "x", "coll": "c"}]}},
        {"$expr": {"$eq": ["$to.coll", "c"]}},
        {"$expr": {"$eq": ["$to.db", "x"]}},
        {"$expr": {"$eq": ["$to", {"db": "x", "coll": "c"}]}},
        {"$expr": {"$eq": ["$fullDocument.a", 1]}},
        {"$expr": {"$eq": ["$fullDocument", {"a": 1}]}},
        {"$expr": {"$eq": ["$fullDocument.a", None]}},
        {"$expr": {"$gt": ["$fullDocument.a", 1]}},
        {"$expr": {"$eq": ["$ns.coll", None]}},
        {"$expr": {"$eq": ["$ns.foo", 1]}},
        {"$expr": {"$eq": ["$to.foo", 1]}},
        {
            "$expr": {
                "$and": [{"$eq": ["$operationType", "insert"]}, {"$eq": ["$fullDocument.a", 1]}]
            }
        },
        {
            "$expr": {
                "$or": [{"$eq": ["$operationType", "insert"]}, {"$eq": ["$fullDocument.a", 1]}]
            }
        },
        {"$expr": {"$or": [{"$eq": ["$operationType", "insert"]}, {"$eq": ["$ns.coll", "c"]}]}},
    ],
)
def test_an_expr_on_the_document_key_or_namespace_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites an `$expr` on `$documentKey` (a `$switch` of
    `o` and `o2`), `$ns` (a `$let` of the database name) and `$to` (a
    `$cond` over `o.to`), and the `$_internalExpr*` twins the optimizer
    derives (as the equality, but on `documentKey` and `fullDocument`), and
    drops an `$expr` on `fullDocument`; this server left all but
    `operationType` out of the read (90A52575)."""
    assert _read_and_unwind(rust_mongo, match) == _read_and_unwind(mongod_rs, match)


@pytest.mark.backlog("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"updateDescription.updatedFields.a": 1},
        {"fullDocumentBeforeChange.a": 1},
        {"txnNumber": 1},
    ],
)
def test_a_match_on_another_event_field_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites a `$match` on `updateDescription`,
    `fullDocumentBeforeChange`, `clusterTime`, `lsid` or `txnNumber` into the
    oplog filter, which changes the read's `queryHash` from 90A52575."""
    want = _query_hash(mongod_rs, match)
    assert want != "90A52575"
    assert _query_hash(rust_mongo, match) == want


def _unwind(client: Any, match: dict, name: str) -> dict:
    return _explain(client, match, name)["stages"][1]["$changeStream"]["filter"]


@pytest.mark.backlog_fixed("RM-699")
@pytest.mark.parametrize("op_type", ["drop", "rename", "dropDatabase"])
def test_the_unwind_filter_is_simplified(rust_mongo: Any, mongod_rs: Any, op_type: str) -> None:
    """mongod 8.2.11 runs its Boolean simplifier over the unwind's filter:
    with `operationType: "drop"` the writes branch (`$nor` of no-ops and
    commands) and the event-type `$or` fall away, since only a command can
    match."""
    match = {"operationType": op_type}
    # One collection name on both sides: the filter spells it out.
    name = "c" + uuid.uuid4().hex[:8]
    want = _unwind(mongod_rs, match, name)
    assert "$nor" not in str(want)
    got = _unwind(rust_mongo, match, name)
    assert got == want
