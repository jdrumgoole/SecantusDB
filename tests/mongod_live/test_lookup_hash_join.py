"""A `$lookup` on `localField` / `foreignField` explained as SBE runs it
(RM-683, slice 1: the hash join).

mongod 8.2 pushes the `$lookup` into its slot-based engine and explains it as
one: `explainVersion` "2", an `EQ_LOOKUP` whose `strategy` is `HashJoin` over
the query's plan, and a `slotBasedPlan` of `hash_lookup`, `nlj`, `branch`,
`union`, `unwind`, `coscan` and `makeBsonObj` stages whose `executionStats`
count every run of the foreign side's key program. Compared are the whole
`queryPlanner`, `executionStats` and classic `stages` of every reply, less
the clock and with each collection's UUID named by the collection, at each
verbosity, over foreign fields that are scalars, arrays, nested paths,
missing and empty, local fields of one to three components, a query in
front, two joins in a row, and a collection that does not exist. Measured on
8.2.11, 2026-11-26.
"""

from __future__ import annotations

import re
from typing import Any

import pytest
from bson import json_util

from tests.mongod_live.conftest import same

_TIMES = ("executionTimeMillisEstimate", "executionTimeMillis", "optimizationTimeMillis")
_UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
_DB = "lookup_hash_join"


def _join(**over: Any) -> dict:
    spec = {"from": "d", "localField": "a", "foreignField": "b", "as": "z"}
    spec.update(over)
    return {"$lookup": spec}


_LOCAL = [{"_id": 1, "a": [1, 2]}, {"_id": 2}, {"_id": 3, "a": 5}]
_FOREIGN = [{"_id": 1, "b": 1}, {"_id": 2, "b": [2, 3]}, {"_id": 3}]
_NESTED_FOREIGN = [
    {"_id": 1, "b": {"c": 1}},
    {"_id": 2, "b": [{"c": 2}, {"c": [3, 4]}]},
    {"_id": 3, "b": []},
    {"_id": 4, "b": [[1]]},
    {"_id": 5, "b": {"c": {"d": 1}}},
    {"_id": 6, "b": {"c": []}},
]
_NESTED_LOCAL = [
    {"_id": 1, "a": {"x": 1}},
    {"_id": 2, "a": [{"x": 2}, {"x": [1, 2]}]},
    {"_id": 3},
    {"_id": 4, "a": {"x": {"y": 1}}},
]
_MANY_LOCAL = [{"_id": i, "a": i % 7} for i in range(100)]
_MANY_FOREIGN = [{"_id": i, "b": i % 5} for i in range(60)]

#: name -> (local documents, foreign documents or None for no collection,
#: pipeline)
CASES: dict[str, tuple[list[dict], list[dict] | None, list[dict]]] = {
    "scalars_and_arrays": (_LOCAL, _FOREIGN, [_join()]),
    "empty_foreign_collection": (_LOCAL, [], [_join()]),
    "no_foreign_collection": (_LOCAL, None, [_join()]),
    "empty_local_collection": ([], _FOREIGN, [_join()]),
    "nested_foreign_field": (_LOCAL, _NESTED_FOREIGN, [_join(foreignField="b.c")]),
    "nested_foreign_field_three": (_LOCAL, _NESTED_FOREIGN, [_join(foreignField="b.c.d")]),
    "nested_local_field": (_NESTED_LOCAL, _FOREIGN, [_join(localField="a.x")]),
    "nested_local_field_three": (_NESTED_LOCAL, _FOREIGN, [_join(localField="a.x.y")]),
    "both_nested": (
        _NESTED_LOCAL,
        _NESTED_FOREIGN,
        [_join(localField="a.x", foreignField="b.c")],
    ),
    "local_id": (_LOCAL, _FOREIGN, [_join(localField="_id")]),
    "as_replaces_a_field": (_LOCAL, _FOREIGN, [_join(**{"as": "a"})]),
    "same_collection": (_LOCAL, None, [_join(**{"from": "c"})]),
    "match_before": (_LOCAL, _FOREIGN, [{"$match": {"a": {"$gt": 1}}}, _join()]),
    "limit_before": (_LOCAL, _FOREIGN, [{"$limit": 2}, _join()]),
    "skip_before": (_LOCAL, _FOREIGN, [{"$skip": 1}, _join()]),
    "two_joins": (_LOCAL, _FOREIGN, [_join(), _join(**{"as": "y"})]),
    "two_joins_nested_first": (
        _NESTED_LOCAL,
        _FOREIGN,
        [_join(localField="a.x"), _join(**{"as": "y"})],
    ),
    "three_joins": (_LOCAL, _FOREIGN, [_join(), _join(**{"as": "y"}), _join(**{"as": "w"})]),
    "match_on_the_joined_array_after": (
        _LOCAL,
        _FOREIGN,
        [_join(), {"$match": {"z.b": 1}}],
    ),
    "add_fields_after": (_LOCAL, _FOREIGN, [_join(), {"$addFields": {"n": {"$size": "$z"}}}]),
    "many_documents": (_MANY_LOCAL, _MANY_FOREIGN, [_join()]),
    "many_documents_and_a_match_after": (
        _MANY_LOCAL,
        _MANY_FOREIGN,
        [_join(), {"$match": {"z.b": 1}}],
    ),
    "many_documents_match_before": (
        _MANY_LOCAL,
        _MANY_FOREIGN,
        [{"$match": {"a": {"$lt": 3}}}, _join()],
    ),
}


def _normal(value: Any, names: dict[str, str]) -> Any:
    """The reply less its clock, its UUIDs named by collection."""
    if isinstance(value, dict):
        return {k: _normal(v, names) for k, v in value.items() if k not in _TIMES}
    if isinstance(value, list):
        return [_normal(v, names) for v in value]
    if isinstance(value, str):
        return _UUID.sub(lambda m: f"<{names.get(m.group(0), '?')}>", value)
    return value


def _explained(client: Any, case: str, verbosity: str) -> str:
    local, foreign, pipeline = CASES[case]
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
    reply = db.command(
        "explain", {"aggregate": "c", "pipeline": pipeline, "cursor": {}}, verbosity=verbosity
    )
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
def test_a_hash_join_lookup_is_explained_as_mongod_does(
    rust_mongo: Any, mongod_rs_no_yield: Any, case: str, verbosity: str
) -> None:
    same(_explained(rust_mongo, case, verbosity), _explained(mongod_rs_no_yield, case, verbosity))


def test_an_explained_hash_join_is_sbe(rust_mongo: Any) -> None:
    reply = _explained(rust_mongo, "scalars_and_arrays", "queryPlanner")
    assert '"explainVersion": "2"' in reply
    assert '"strategy": "HashJoin"' in reply
