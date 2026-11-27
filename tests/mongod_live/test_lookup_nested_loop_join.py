"""A `$lookup` on `localField` / `foreignField` that mongod joins by a nested
loop, explained as SBE runs it (RM-683, slice 3).

mongod 8.2 joins by `NestedLoopJoin` when no index qualifies (see
`test_lookup_indexed_loop_join.py`) and a hash join is not taken: the command
has `allowDiskUse: false`, or the foreign collection holds more than 10,000
documents or 100 MiB. The plan is the indexed loop join's without its index:
`nlj` over the local rows and, for each, a `scan` of the whole foreign
collection under the `filter`, `group` and `union` that keep the documents
whose keys match. Compared are the whole `queryPlanner`, `executionStats` and
classic `stages` of every reply, less the clock and with each collection's UUID
named by the collection, at each verbosity, against a mongod whose time-based
plan yielding is off (`mongod_rs_no_yield`: its yields are timed and this
server makes none). Measured on 8.2.11, 2026-11-27.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import json_util

from tests.mongod_live.conftest import same
from tests.mongod_live.test_lookup_indexed_loop_join import (
    _B,
    _FOREIGN,
    _LOCAL,
    _MANY_FOREIGN,
    _MANY_LOCAL,
    _NESTED_FOREIGN,
    _NESTED_LOCAL,
    _join,
    _normal,
)

_DB = "lookup_nested_loop_join"
_NO_DISK = {"allowDiskUse": False}

#: name -> (local documents, foreign documents or None for no collection,
#: indexes as (key, options), pipeline)
CASES: dict[str, tuple[list[dict], list[dict] | None, list, list[dict]]] = {
    "scalars_and_arrays": (_LOCAL, _FOREIGN, [], [_join()]),
    "empty_foreign_collection": (_LOCAL, [], [], [_join()]),
    "no_foreign_collection": (_LOCAL, None, [], [_join()]),
    "empty_local_collection": ([], _FOREIGN, [], [_join()]),
    "nested_foreign_field": (_LOCAL, _NESTED_FOREIGN, [], [_join(foreignField="b.c")]),
    "nested_foreign_field_three": (_LOCAL, _NESTED_FOREIGN, [], [_join(foreignField="b.c.d")]),
    "nested_local_field": (_NESTED_LOCAL, _FOREIGN, [], [_join(localField="a.x")]),
    "both_nested": (
        _NESTED_LOCAL,
        _NESTED_FOREIGN,
        [],
        [_join(localField="a.x", foreignField="b.c")],
    ),
    "local_id": (_LOCAL, _FOREIGN, [], [_join(localField="_id")]),
    "as_replaces_a_field": (_LOCAL, _FOREIGN, [], [_join(**{"as": "a"})]),
    "match_before": (_LOCAL, _FOREIGN, [], [{"$match": {"a": {"$gt": 1}}}, _join()]),
    "limit_before": (_LOCAL, _FOREIGN, [], [{"$limit": 2}, _join()]),
    "two_joins": (_LOCAL, _FOREIGN, [], [_join(), _join(**{"as": "y"})]),
    "an_indexed_join_then_a_nested_one": (
        _LOCAL,
        _FOREIGN,
        [([("b", 1)], {})],
        [_join(), _join(**{"from": "c", "foreignField": "a", "as": "y"})],
    ),
    "match_on_the_joined_array_after": (
        _LOCAL,
        _FOREIGN,
        [],
        [_join(), {"$match": {"z.b": 1}}],
    ),
    "many_documents": (_MANY_LOCAL, _MANY_FOREIGN, [], [_join()]),
    "sparse_index": (_LOCAL, _FOREIGN, [(_B, {"sparse": True})], [_join()]),
    "partial_index": (
        _LOCAL,
        _FOREIGN,
        [(_B, {"partialFilterExpression": {"b": {"$gt": 0}}})],
        [_join()],
    ),
    "hidden_index": (_LOCAL, _FOREIGN, [(_B, {"hidden": True})], [_join()]),
    "index_on_another_field": (_LOCAL, _FOREIGN, [([("x", 1)], {})], [_join()]),
}


def explained(
    client: Any, name: str, verbosity: str, case: Any = None, options: dict | None = None
) -> str:
    local, foreign, indexes, pipeline = case or CASES[name]
    client.drop_database(_DB)
    db = client[_DB]
    if local:
        db.c.insert_many(local)
    else:
        db.create_collection("c")
    if foreign:
        db.d.insert_many(foreign)
    elif foreign is not None:
        db.create_collection("d")
    for key, index_options in indexes:
        db.d.create_index(key, **index_options)
    command = {
        "aggregate": "c",
        "pipeline": pipeline,
        "cursor": {},
        **(_NO_DISK if options is None else options),
    }
    reply = db.command("explain", command, verbosity=verbosity)
    names = {
        str(c["info"]["uuid"].as_uuid()): c["name"]
        for c in db.command("listCollections")["cursor"]["firstBatch"]
    }
    kept = {
        k: reply[k]
        for k in ("explainVersion", "queryPlanner", "executionStats", "stages")
        if k in reply
    }
    return json_util.dumps(_normal(kept, names), sort_keys=True)


@pytest.mark.parametrize("verbosity", ["queryPlanner", "executionStats", "allPlansExecution"])
@pytest.mark.parametrize("case", list(CASES))
def test_a_nested_loop_join_lookup_is_explained_as_mongod_does(
    rust_mongo: Any, mongod_rs_no_yield: Any, case: str, verbosity: str
) -> None:
    same(explained(rust_mongo, case, verbosity), explained(mongod_rs_no_yield, case, verbosity))


#: A foreign collection past 10,000 documents, with and without an index
#: that does not qualify.
_BIG = [{"_id": n, "b": n % 7} for n in range(10_001)]


@pytest.mark.parametrize("verbosity", ["queryPlanner", "executionStats"])
@pytest.mark.parametrize(
    "indexes", [[], [(_B, {"sparse": True})]], ids=["no_index", "a_sparse_index"]
)
def test_a_lookup_over_a_large_collection_is_a_nested_loop_join(
    rust_mongo: Any, mongod_rs_no_yield: Any, indexes: list, verbosity: str
) -> None:
    case = (_LOCAL, _BIG, indexes, [_join()])
    same(
        explained(rust_mongo, "big", verbosity, case, {}),
        explained(mongod_rs_no_yield, "big", verbosity, case, {}),
    )


def test_an_explained_nested_loop_join_is_sbe(rust_mongo: Any) -> None:
    reply = explained(rust_mongo, "scalars_and_arrays", "queryPlanner")
    assert '"explainVersion": "2"' in reply
    assert '"strategy": "NestedLoopJoin"' in reply
