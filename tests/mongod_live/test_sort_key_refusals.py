"""The documents mongod cannot build a sort key for (RM-332), compared over
every way a client sorts.

mongod builds a sort key with its B-tree key generator, which refuses two
sort paths that meet different arrays of one document (2 "cannot sort with
keys that are parallel arrays") and a component that is both an index of an
array and a key of one of its elements (16746). `$sort`, `find`,
`findAndModify`, `update` and `$setWindowFields` refuse alike; the `$top` /
`$bottom` accumulators only when two or more paths meet an array, and then
differently in each engine; a window function that needs a one-field
`sortBy` refuses a longer one first. Each shape holds ONE document, since a lone
document's key is built too. Measured on 8.2.1, 2026-11-08.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same

SHAPES: list[tuple[dict, dict]] = [
    ({"a": [1, 2], "b": [3, 4]}, {"a": 1, "b": 1}),
    ({"a": [1, 2], "b": [3, 4]}, {"a": -1, "b": -1}),
    ({"a": [1, 2], "b": 3}, {"a": 1, "b": 1}),
    ({"a": [], "b": []}, {"a": 1, "b": 1}),
    ({"a": {"b": [1], "c": [2]}}, {"a.b": 1, "a.c": 1}),
    ({"a": [{"x": 1, "y": 2}, {"x": 3, "y": 0}]}, {"a.x": 1, "a.y": 1}),
    ({"a": [{"b": [1, 2], "c": [3, 4]}]}, {"a.b": 1, "a.c": 1}),
    ({"a": [{"b": {"c": [1, 2], "d": [3]}}]}, {"a.b.c": 1, "a.b.d": 1}),
    ({"a": [{"b": [1]}, {"c": [2]}]}, {"a.b": 1, "a.c": 1}),
    ({"a": [{"b": 1}]}, {"a": 1, "a.b": 1}),
    ({"a": {"b": [1]}}, {"a": 1, "a.b": 1}),
    ({"a": None, "b": [1]}, {"a": 1, "b": 1}),
    ({"a": [[1, 2], [3, 4]]}, {"a.0": 1, "a.1": 1}),
    ({"a": [5, 6], "b": 1}, {"a.0": 1, "a.1": 1}),
    ({"a": [{"c": [1]}, {"c": [2]}]}, {"a.0.c": 1, "a.c": 1}),
    ({"a": [[[1], [2]]]}, {"a.0.0": 1, "a.0.1": 1}),
    ({"a": [{"0": 1}], "b": []}, {"a.0": 1, "b.d": 1}),
    ({"a": [{"0": [1]}, {"0": [2]}]}, {"a.0": 1, "b": 1}),
    ({"a": [{"0": [1]}, {"z": 1}]}, {"a.0": 1, "a.z": 1}),
    ({"a": [{"0": 1, "x": [1], "y": [2]}]}, {"a.0": 1, "a.x": 1, "a.y": 1}),
    ({"a": [{"0": 1, "x": [1], "y": [2]}]}, {"a.x": 1, "a.y": 1, "a.0": 1}),
    ({"x": [{"a": 5}, {"0": 6}]}, {"x.0": 1}),
]

# Two `$project`s ahead of a `$group` keep it in the classic engine: the find
# layer takes one, and nothing is pushed down past the other.
_CLASSIC = [{"$project": {"a": 1, "b": 1, "x": 1}}] * 2


def _sorted(client: Any, entry: str, doc: dict, spec: dict) -> Any:
    db = client.rm332live
    c = db.c
    c.drop()
    c.insert_one(dict(doc, _id=1))
    top: dict = {"n": 1, "sortBy": spec, "output": "$_id"}
    if entry == "$sort":
        return list(c.aggregate([{"$sort": spec}]))
    if entry == "find":
        return list(c.find({}, sort=list(spec.items())))
    if entry == "findAndModify":
        return c.find_one_and_update({}, {"$set": {"z": 1}}, sort=list(spec.items()))
    if entry == "update":
        reply = db.command(
            {"update": "c", "updates": [{"q": {}, "u": {"$set": {"z": 1}}, "sort": spec}]}
        )
        return {k: reply.get(k) for k in ("n", "nModified", "writeErrors")}
    if entry == "$setWindowFields":
        window = {"$sum": 1, "window": {"documents": ["unbounded", "current"]}}
        return list(c.aggregate([{"$setWindowFields": {"sortBy": spec, "output": {"w": window}}}]))
    if entry == "$documentNumber":
        # Refused for a sortBy of more than one field before any document is
        # read, so before the `$sort` it runs behind can refuse one.
        number = {"n": {"$documentNumber": {}}}
        return list(c.aggregate([{"$setWindowFields": {"sortBy": spec, "output": number}}]))
    if entry == "$topN sbe":
        return list(c.aggregate([{"$group": {"_id": None, "t": {"$topN": top}}}]))
    assert entry == "$bottomN classic"
    return list(c.aggregate([*_CLASSIC, {"$group": {"_id": None, "t": {"$bottomN": top}}}]))


@pytest.mark.parametrize(
    "entry",
    [
        "$sort",
        "find",
        "findAndModify",
        "update",
        "$setWindowFields",
        "$documentNumber",
        "$topN sbe",
        "$bottomN classic",
    ],
)
def test_sort_key_refusals_match_mongod(rust_mongo: Any, mongod: Any, entry: str) -> None:
    for doc, spec in SHAPES:
        ours = mongo_answer(lambda d=doc, s=spec: _sorted(rust_mongo, entry, d, s))
        theirs = mongo_answer(lambda d=doc, s=spec: _sorted(mongod, entry, d, s))
        same(
            {"doc": doc, "sort": spec, "answer": ours}, {"doc": doc, "sort": spec, "answer": theirs}
        )
