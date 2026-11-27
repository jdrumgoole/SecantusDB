"""A `$lookup` on `localField` / `foreignField` whose foreign field leads an
index, explained as SBE runs it (RM-683, slice 2: the indexed loop join).

mongod 8.2 picks `IndexedLoopJoin` when an index of the foreign collection
leads with the foreign field and is a btree or hashed index that is not
sparse, not partial, not hidden and whose collation is the query's (`_id`
counts); of those it takes the one with the fewest fields, btree before
hashed, then the lesser key pattern. The plan is `nlj` over the local
query and, for each row, an `ixseek` of the local keys' intervals of the
index and a `seek` of the documents, whose `executionStats` count every
seek and key. A collated, sparse or partial index does not qualify and the
`$lookup` stays a hash join (`test_lookup_hash_join.py`). Compared are the
whole `queryPlanner`, `executionStats` and classic `stages` of every reply,
less the clock and with each collection's UUID named by the collection, at
each verbosity. Measured on 8.2.11, 2026-11-27.
"""

from __future__ import annotations

import re
from datetime import datetime
from typing import Any

import pytest
from bson import Int64, json_util

from tests.mongod_live.conftest import same

_TIMES = ("executionTimeMillisEstimate", "executionTimeMillis", "optimizationTimeMillis")
_UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
_DB = "lookup_indexed_loop_join"


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
_ARRAYS_LOCAL = [
    {"_id": 1, "a": [[1, 2]]},
    {"_id": 2, "a": [[1, 2], 3]},
    {"_id": 3, "a": []},
    {"_id": 4, "a": [1, [1, 2]]},
    {"_id": 5, "a": None},
    {"_id": 6, "a": "x"},
    {"_id": 7, "a": {"k": 1}},
]
_ARRAYS_FOREIGN = [
    {"_id": 1, "b": [1, 2]},
    {"_id": 2, "b": [[1, 2], 42]},
    {"_id": 3, "b": []},
    {"_id": 4, "b": None},
    {"_id": 5, "b": "x"},
    {"_id": 6, "b": {"k": 1}},
    {"_id": 7, "b": 3},
    {"_id": 8},
]
_SCALAR_FOREIGN = [
    {"_id": 1, "b": 1, "x": 1},
    {"_id": 2, "b": 2, "x": 2},
    {"_id": 3, "x": 3},
    {"_id": 4, "b": None},
    {"_id": 5, "b": "x"},
    {"_id": 6, "b": {"k": 1}},
    {"_id": 7, "b": 2},
]
_MANY_LOCAL = [{"_id": i, "a": i % 7} for i in range(100)]
_MANY_FOREIGN = [{"_id": i, "b": i % 5, "x": i} for i in range(60)]

_MIXED_LOCAL = [
    {"_id": 1, "a": 1},
    {"_id": 2, "a": 2.0},
    {"_id": 3, "a": Int64(3)},
    {"_id": 4, "a": 4},
    {"_id": 5, "a": float("nan")},
    {"_id": 6, "a": -0.0},
    {"_id": 7, "a": "x"},
    {"_id": 8, "a": True},
    {"_id": 9, "a": datetime(2020, 1, 1)},
    {"_id": 10, "a": [1, "x", None]},
]
_MIXED_FOREIGN = [
    {"_id": 1, "b": 1.0},
    {"_id": 2, "b": 2},
    {"_id": 3, "b": 3.0},
    {"_id": 4, "b": 4},
    {"_id": 5, "b": float("nan")},
    {"_id": 6, "b": 0},
    {"_id": 7, "b": "x"},
    {"_id": 8, "b": True},
    {"_id": 9, "b": datetime(2020, 1, 1)},
    {"_id": 10, "b": [1, 2, 2, "x"]},
    {"_id": 11, "b": [None, None]},
]
_UNIQUE_FOREIGN = [{"_id": n, "b": n, "x": n % 2} for n in range(1, 6)]
_B = [("b", 1)]
_HASHED = [("b", "hashed")]

#: name -> (local documents, foreign documents or None for no collection,
#: indexes as (key, options), pipeline)
CASES: dict[str, tuple[list[dict], list[dict] | None, list, list[dict]]] = {
    "btree_scalars_and_arrays": (_LOCAL, _FOREIGN, [(_B, {})], [_join()]),
    "btree_descending": (_LOCAL, _FOREIGN, [([("b", -1)], {})], [_join()]),
    "hashed": (_LOCAL, _SCALAR_FOREIGN, [(_HASHED, {})], [_join()]),
    "compound_led_by_it": (_LOCAL, _FOREIGN, [([("b", 1), ("x", 1)], {})], [_join()]),
    "compound_second_descending": (_LOCAL, _FOREIGN, [([("b", 1), ("x", -1)], {})], [_join()]),
    "compound_hashed_second": (
        _LOCAL,
        _SCALAR_FOREIGN,
        [([("b", 1), ("x", "hashed")], {})],
        [_join()],
    ),
    "compound_hashed_first": (
        _LOCAL,
        _SCALAR_FOREIGN,
        [([("b", "hashed"), ("x", 1)], {})],
        [_join()],
    ),
    "foreign_id": (_LOCAL, _FOREIGN, [], [_join(foreignField="_id")]),
    "foreign_id_local_id": (_LOCAL, _FOREIGN, [], [_join(localField="_id", foreignField="_id")]),
    "multikey": (_LOCAL, _ARRAYS_FOREIGN, [(_B, {})], [_join()]),
    "array_values_of_the_local_field": (_ARRAYS_LOCAL, _ARRAYS_FOREIGN, [(_B, {})], [_join()]),
    "array_values_hashed": (_ARRAYS_LOCAL, _SCALAR_FOREIGN, [(_HASHED, {})], [_join()]),
    "nested_foreign_field": (
        _LOCAL,
        _NESTED_FOREIGN,
        [([("b.c", 1)], {})],
        [_join(foreignField="b.c")],
    ),
    "nested_foreign_field_three": (
        _LOCAL,
        _NESTED_FOREIGN,
        [([("b.c.d", 1)], {})],
        [_join(foreignField="b.c.d")],
    ),
    "nested_local_field": (_NESTED_LOCAL, _FOREIGN, [(_B, {})], [_join(localField="a.x")]),
    "both_nested": (
        _NESTED_LOCAL,
        _NESTED_FOREIGN,
        [([("b.c", 1)], {})],
        [_join(localField="a.x", foreignField="b.c")],
    ),
    "empty_foreign_collection": (_LOCAL, [], [(_B, {})], [_join()]),
    "empty_local_collection": ([], _FOREIGN, [(_B, {})], [_join()]),
    "as_replaces_a_field": (_LOCAL, _FOREIGN, [(_B, {})], [_join(**{"as": "a"})]),
    "same_collection": (_LOCAL, None, [], [_join(**{"from": "c", "foreignField": "_id"})]),
    "match_before": (_LOCAL, _FOREIGN, [(_B, {})], [{"$match": {"a": {"$gt": 1}}}, _join()]),
    "limit_before": (_LOCAL, _FOREIGN, [(_B, {})], [{"$limit": 2}, _join()]),
    "skip_before": (_LOCAL, _FOREIGN, [(_B, {})], [{"$skip": 1}, _join()]),
    "two_joins": (_LOCAL, _FOREIGN, [(_B, {})], [_join(), _join(**{"as": "y"})]),
    "match_on_the_joined_array_after": (
        _LOCAL,
        _FOREIGN,
        [(_B, {})],
        [_join(), {"$match": {"z.b": 1}}],
    ),
    "add_fields_after": (
        _LOCAL,
        _FOREIGN,
        [(_B, {})],
        [_join(), {"$addFields": {"n": {"$size": "$z"}}}],
    ),
    "many_documents": (_MANY_LOCAL, _MANY_FOREIGN, [(_B, {})], [_join()]),
    "many_documents_hashed": (_MANY_LOCAL, _MANY_FOREIGN, [(_HASHED, {})], [_join()]),
    "many_documents_match_before": (
        _MANY_LOCAL,
        _MANY_FOREIGN,
        [(_B, {})],
        [{"$match": {"a": {"$lt": 3}}}, _join()],
    ),
    "mixed_types": (_MIXED_LOCAL, _MIXED_FOREIGN, [(_B, {})], [_join()]),
    "mixed_types_descending": (_MIXED_LOCAL, _MIXED_FOREIGN, [([("b", -1)], {})], [_join()]),
    "unique_index": (_LOCAL, _UNIQUE_FOREIGN, [(_B, {"unique": True})], [_join()]),
    "unique_compound_index": (
        _LOCAL,
        _UNIQUE_FOREIGN,
        [([("b", 1), ("x", 1)], {"unique": True})],
        [_join()],
    ),
    "duplicate_keys_in_a_document": (
        _LOCAL,
        [{"_id": 1, "b": [2, 2, 1]}, {"_id": 2, "b": [1, 1]}],
        [(_B, {})],
        [_join()],
    ),
    "past_10000_documents": (
        _LOCAL,
        [{"_id": n, "b": n} for n in range(10_001)],
        [(_B, {})],
        [_join()],
    ),
    # The index that is picked among several.
    "fewer_fields_win": (
        _LOCAL,
        _FOREIGN,
        [([("b", 1), ("x", 1)], {}), (_B, {})],
        [_join()],
    ),
    "btree_before_hashed": (_LOCAL, _SCALAR_FOREIGN, [(_HASHED, {}), (_B, {})], [_join()]),
    "fewer_fields_beat_btree": (
        _LOCAL,
        _SCALAR_FOREIGN,
        [([("b", 1), ("x", 1)], {}), (_HASHED, {})],
        [_join()],
    ),
    "lesser_key_pattern": (
        _LOCAL,
        _FOREIGN,
        [(_B, {}), ([("b", -1)], {})],
        [_join()],
    ),
    # Indexes that do not qualify: a hash join as before.
    "sparse_index": (_LOCAL, _FOREIGN, [(_B, {"sparse": True})], [_join()]),
    "partial_index": (
        _LOCAL,
        _FOREIGN,
        [(_B, {"partialFilterExpression": {"b": {"$gt": 0}}})],
        [_join()],
    ),
    "hidden_index": (_LOCAL, _FOREIGN, [(_B, {"hidden": True})], [_join()]),
    "collated_index": (_LOCAL, _FOREIGN, [(_B, {"collation": {"locale": "en"}})], [_join()]),
    "index_on_another_field": (_LOCAL, _FOREIGN, [([("x", 1)], {})], [_join()]),
    "foreign_field_second_in_the_index": (
        _LOCAL,
        _FOREIGN,
        [([("x", 1), ("b", 1)], {})],
        [_join()],
    ),
    "geo_index_on_the_field": (
        _LOCAL,
        [{"_id": 1, "b": {"type": "Point", "coordinates": [1, 1]}}, {"_id": 2, "x": 1}],
        [([("b", "2dsphere")], {})],
        [_join()],
    ),
    "collated_index_beside_a_plain_one": (
        _LOCAL,
        _FOREIGN,
        [(_B, {"collation": {"locale": "en"}}), ([("b", 1), ("x", 1)], {})],
        [_join()],
    ),
    "collated_index_beside_a_hashed_one": (
        _LOCAL,
        _SCALAR_FOREIGN,
        [(_B, {"collation": {"locale": "en"}}), (_HASHED, {})],
        [_join()],
    ),
    "no_disk_use": (_LOCAL, _FOREIGN, [(_B, {})], [_join()]),
    "a_qualifying_index_beside_a_sparse_one": (
        _LOCAL,
        _FOREIGN,
        [(_B, {"sparse": True}), ([("b", 1), ("x", 1)], {})],
        [_join()],
    ),
}


#: Options of the command a case runs with. An indexed loop join needs no
#: `allowDiskUse`, which a hash join does.
_OPTIONS: dict[str, dict] = {"no_disk_use": {"allowDiskUse": False}}


def _normal(value: Any, names: dict[str, str]) -> Any:
    """The reply less its clock, its UUIDs named by collection."""
    if isinstance(value, dict):
        return {k: _normal(v, names) for k, v in value.items() if k not in _TIMES}
    if isinstance(value, list):
        return [_normal(v, names) for v in value]
    if isinstance(value, str):
        return _UUID.sub(lambda m: f"<{names.get(m.group(0), '?')}>", value)
    return value


def explained(client: Any, case: str, verbosity: str) -> str:
    local, foreign, indexes, pipeline = CASES[case]
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
    for key, options in indexes:
        db.d.create_index(key, **options)
    command = {"aggregate": "c", "pipeline": pipeline, "cursor": {}, **_OPTIONS.get(case, {})}
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
def test_an_indexed_loop_join_lookup_is_explained_as_mongod_does(
    rust_mongo: Any, mongod_rs: Any, case: str, verbosity: str
) -> None:
    same(explained(rust_mongo, case, verbosity), explained(mongod_rs, case, verbosity))


def test_an_explained_indexed_loop_join_is_sbe(rust_mongo: Any) -> None:
    reply = explained(rust_mongo, "btree_scalars_and_arrays", "queryPlanner")
    assert '"explainVersion": "2"' in reply
    assert '"strategy": "IndexedLoopJoin"' in reply
