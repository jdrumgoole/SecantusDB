"""Cursors that read on as `getMore` asks (RM-080), against a live mongod.

Each scenario opens a cursor, takes its first batch, writes, and reads the
rest: mongod's executor saves its position between batches and restores it
on a fresh snapshot, so what a later batch returns depends on where each
write landed against the position. The Rust server must return the same
documents in the same order: descending, multikey and collated indexes,
`$or` and `SORT_MERGE` plans, `min` / `max`, `returnKey` / `showRecordId`, a
sort of index keys below the FETCH, the `_id` index over document `_id`s,
and `aggregate` under `notablescan` and with `{$meta: "indexKey"}`.
"""

from __future__ import annotations

import uuid
from collections.abc import Callable
from typing import Any

import pytest

from tests.mongod_live.conftest import same

COLLATION = {"locale": "en", "strength": 2}


def _coll(client: Any) -> Any:
    return client["rm080_live"]["c" + uuid.uuid4().hex[:10]]


def _read(cur: Any, first: int, writes: Callable[[], None]) -> list:
    """The cursor's first `first` documents, the writes, then the rest."""
    out = [next(cur) for _ in range(first)]
    writes()
    return out + list(cur)


def _ids(docs: list) -> list:
    return [d["_id"] for d in docs]


def desc_strings_forward(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "s": v} for i, v in enumerate(["a", "ab", "abc", "b", "ba", "c"])])
    c.create_index([("s", -1)])

    def writes() -> None:
        c.insert_one({"_id": 10, "s": "bb"})
        c.insert_one({"_id": 11, "s": "aba"})
        c.delete_one({"s": "ab"})
        c.update_one({"s": "a"}, {"$set": {"s": "bz"}})
        c.update_one({"s": "c"}, {"$set": {"s": "a0"}})

    return [d["s"] for d in _read(c.find({"s": {"$gte": ""}}).batch_size(2), 2, writes)]


def desc_strings_backward(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many(
        [{"_id": i, "s": v} for i, v in enumerate(["a", "ab", "abc", "b", "ba", "c", ""])]
    )
    c.create_index([("s", -1)])

    def writes() -> None:
        c.insert_one({"_id": 11, "s": "aa"})
        c.insert_one({"_id": 12, "s": ""})

    return [d["s"] for d in _read(c.find().sort("s", 1).batch_size(2), 2, writes)]


def desc_compound(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many(
        [{"_id": i, "a": i % 2, "s": v} for i, v in enumerate(["x", "xy", "y", "", "xa", "z"])]
    )
    c.create_index([("a", 1), ("s", -1)])

    def writes() -> None:
        c.insert_one({"_id": 20, "a": 1, "s": "x"})
        c.insert_one({"_id": 21, "a": 0, "s": "zz"})

    cur = c.find({"a": {"$in": [0, 1]}, "s": {"$lte": "y"}}).batch_size(2)
    return _ids(_read(cur, 2, writes))


def multikey_forward(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many(
        [
            {"_id": 1, "t": ["a", "z"]},
            {"_id": 2, "t": ["b"]},
            {"_id": 3, "t": ["c", "d"]},
            {"_id": 4, "t": "e"},
            {"_id": 5, "t": ["f", "a"]},
        ]
    )
    c.create_index("t")

    def writes() -> None:
        c.update_one({"_id": 2}, {"$push": {"t": "y"}})
        c.insert_one({"_id": 6, "t": ["c", "q"]})
        c.update_one({"_id": 4}, {"$set": {"t": ["0"]}})

    return _ids(_read(c.find({"t": {"$gte": "a"}}).batch_size(2), 2, writes))


def multikey_backward_sort(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "t": [i, 10 - i]} for i in range(1, 6)])
    c.create_index("t")

    def writes() -> None:
        c.insert_one({"_id": 7, "t": [0, 3]})
        c.update_one({"_id": 1}, {"$set": {"t": [1]}})

    return _ids(_read(c.find().sort("t", -1).batch_size(2), 2, writes))


def multikey_shapes(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many(
        [
            {"_id": 1, "t": []},
            {"_id": 2, "t": [[1], 2]},
            {"_id": 3, "t": [3, [0]]},
            {"_id": 4},
            {"_id": 5, "t": None},
            {"_id": 6, "t": [[]]},
        ]
    )
    c.create_index("t")

    def writes() -> None:
        c.insert_one({"_id": 7, "t": []})
        c.insert_one({"_id": 8, "t": [[2]]})

    return _ids(_read(c.find().hint("t_1").batch_size(1), 1, writes))


def multikey_missing_leaf(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many(
        [
            {"_id": 1, "a": [{"b": 5}, {"c": 1}]},
            {"_id": 2, "a": [{"b": 1}]},
            {"_id": 3, "a": {"b": 3}},
            {"_id": 4, "a": [{"b": 2}, {"b": 7}]},
            {"_id": 5},
        ]
    )
    c.create_index("a.b")

    def writes() -> None:
        c.insert_one({"_id": 6, "a": [{"c": 1}]})
        c.insert_one({"_id": 7, "a": [{"b": 6}]})

    return _ids(_read(c.find().sort("a.b", 1).hint("a.b_1").batch_size(1), 1, writes))


def multikey_compound(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i % 2, "t": [i, 20 - i]} for i in range(8)])
    c.create_index([("a", 1), ("t", -1)])
    cur = c.find({"a": {"$in": [0, 1]}, "t": {"$gt": 3}}).batch_size(3)
    first = [next(cur) for _ in range(3)]
    c.insert_one({"_id": 20, "a": 0, "t": [4]})
    c.update_one({"_id": first[0]["_id"]}, {"$set": {"t": [100]}})
    return _ids(first + list(cur))


def collated_strings(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many(
        [{"_id": i, "s": v} for i, v in enumerate(["b", "A", "a", "B", "é", "e", "ab", "Ab", "c"])]
    )
    c.create_index("s", collation=COLLATION)

    def writes() -> None:
        c.insert_one({"_id": 20, "s": "AB"})
        c.insert_one({"_id": 21, "s": "a"})
        c.insert_one({"_id": 22, "s": "d"})

    cur = c.find({"s": {"$gte": "a"}}, collation=COLLATION).batch_size(3)
    return _ids(_read(cur, 3, writes))


def collated_strings_descending(cl: Any) -> list:
    c = _coll(cl)
    collation = {"locale": "en", "strength": 1}
    c.insert_many(
        [{"_id": i, "s": v} for i, v in enumerate(["b", "A", "a", "B", "é", "e", "ab", "Ab", "c"])]
    )
    c.create_index([("s", -1)], collation=collation)

    def writes() -> None:
        c.insert_one({"_id": 20, "s": "AB"})
        c.insert_one({"_id": 22, "s": "d"})
        c.insert_one({"_id": 23, "s": "aa"})

    cur = c.find({}, collation=collation).sort("s", -1).batch_size(3)
    return _ids(_read(cur, 3, writes))


def or_plan(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i, "b": 10 - i} for i in range(10)])
    c.create_index("a")
    c.create_index("b")

    def writes() -> None:
        c.insert_one({"_id": 20, "a": 3.5, "b": 100})
        c.insert_one({"_id": 21, "a": -1, "b": 100})
        c.update_one({"_id": 0}, {"$set": {"b": 0}})
        c.insert_one({"_id": 22, "a": 100, "b": 1})

    cur = c.find({"$or": [{"a": {"$lt": 4}}, {"b": {"$lt": 3}}]}).batch_size(2)
    return _ids(_read(cur, 2, writes))


def or_beside_a_clause(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i, "b": 10 - i, "c": i % 2} for i in range(10)])
    c.create_index("a")
    c.create_index("b")

    def writes() -> None:
        c.insert_one({"_id": 20, "a": 3.5, "b": 100, "c": 0})
        c.insert_one({"_id": 22, "a": 100, "b": 1, "c": 0})

    cur = c.find({"c": 0, "$or": [{"a": {"$lt": 4}}, {"b": {"$lt": 3}}]}).batch_size(1)
    return _ids(_read(cur, 1, writes))


def or_scanning_the_collection(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i, "b": 10 - i} for i in range(10)])
    c.create_index("a")

    def writes() -> None:
        c.insert_one({"_id": 20, "a": 3.5, "b": 100})
        c.insert_one({"_id": 22, "a": 100, "b": 1})

    cur = c.find({"$or": [{"a": {"$lt": 4}}, {"b": {"$lt": 3}}]}).batch_size(2)
    return _ids(_read(cur, 2, writes))


def sort_merge_of_an_in(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i % 3, "c": i} for i in range(12)])
    c.create_index([("a", 1), ("c", 1)])

    def writes() -> None:
        c.insert_one({"_id": 20, "a": 0, "c": 2.5})
        c.insert_one({"_id": 21, "a": 1, "c": 0.5})
        c.delete_one({"_id": 4})

    return _ids(_read(c.find({"a": {"$in": [0, 1]}}).sort("c", 1).batch_size(2), 2, writes))


def sort_merge_of_an_or(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i % 3, "b": i % 2, "c": i} for i in range(12)])
    c.create_index([("a", 1), ("c", 1)])
    c.create_index([("b", 1), ("c", 1)])

    def writes() -> None:
        c.insert_one({"_id": 20, "a": 0, "b": 1, "c": 4.5})
        c.update_one({"_id": 1}, {"$set": {"c": 100}})

    cur = c.find({"$or": [{"a": 0}, {"b": 1}]}).sort("c", 1).batch_size(2)
    return _ids(_read(cur, 2, writes))


def sort_merge_with_a_filtered_branch(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i % 3, "b": i % 2, "c": i, "d": i % 4} for i in range(16)])
    c.create_index([("a", 1), ("c", 1)])
    c.create_index([("b", 1), ("c", 1)])

    def writes() -> None:
        c.insert_one({"_id": 20, "a": 0, "b": 0, "c": 6.5, "d": 1})
        c.delete_one({"_id": 9})

    cur = c.find({"$or": [{"a": 0, "d": {"$ne": 0}}, {"b": 1}]}).sort("c", -1).batch_size(2)
    return _ids(_read(cur, 2, writes))


def min_max(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i} for i in range(10)])
    c.create_index("x")

    def writes() -> None:
        c.insert_one({"_id": 20, "x": 5.5})
        c.insert_one({"_id": 21, "x": 8})
        c.delete_one({"_id": 6})

    cur = c.find().hint("x_1").min([("x", 2)]).max([("x", 8)]).batch_size(2)
    return _ids(_read(cur, 2, writes))


def min_max_backward(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i} for i in range(10)])
    c.create_index("x")

    def writes() -> None:
        c.insert_one({"_id": 20, "x": 5.5})
        c.insert_one({"_id": 21, "x": 2})
        c.delete_one({"_id": 4})

    cur = c.find().hint("x_1").min([("x", 2)]).max([("x", 8)]).sort("x", -1).batch_size(2)
    return _ids(_read(cur, 2, writes))


def min_max_compound_descending(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i % 3, "y": i} for i in range(12)])
    c.create_index([("x", 1), ("y", -1)])

    def writes() -> None:
        c.insert_one({"_id": 20, "x": 1, "y": 0.5})
        c.insert_one({"_id": 21, "x": 0, "y": 7})

    cur = (
        c.find().hint("x_1_y_-1").min([("x", 0), ("y", 6)]).max([("x", 2), ("y", 5)]).batch_size(2)
    )
    return _ids(_read(cur, 2, writes))


def return_key(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i} for i in range(6)])
    c.create_index("x")

    def writes() -> None:
        c.insert_one({"_id": 20, "x": 4.5})
        c.update_one({"_id": 5}, {"$set": {"x": 0}})

    return _read(c.find({"x": {"$gte": 1}}, return_key=True).batch_size(2), 2, writes)


def return_key_multikey(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "t": [i, i + 10]} for i in range(4)])
    c.create_index("t")

    def writes() -> None:
        c.insert_one({"_id": 20, "t": [2.5]})

    return _read(c.find({"t": {"$gte": 1}}, return_key=True).batch_size(2), 2, writes)


def return_key_collection_scan(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i} for i in range(4)])

    def writes() -> None:
        c.insert_one({"_id": 20})

    return _read(c.find({}, return_key=True).batch_size(2), 2, writes)


def show_record_id(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i} for i in range(6)])
    c.create_index("x")

    def writes() -> None:
        c.insert_one({"_id": 20, "x": 4.5})

    docs = _read(c.find({"x": {"$gte": 1}}, show_record_id=True).batch_size(2), 2, writes)
    return [(d["_id"], type(d["$recordId"]).__name__) for d in docs]


def _keys_sort(cl: Any, filter_: dict, sort: list) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "a": i % 3, "b": i % 2, "c": 20 - i, "z": 0} for i in range(12)])
    c.create_index([("a", 1), ("c", 1)])
    c.create_index([("b", 1), ("c", 1)])

    def writes() -> None:
        c.delete_one({"_id": 5})
        c.update_one({"_id": 4}, {"$set": {"z": 1}})
        c.update_one({"_id": 7}, {"$set": {"c": 100, "z": 2}})
        c.insert_one({"_id": 50, "a": 1, "b": 1, "c": 0, "z": 3})

    docs = _read(c.find(filter_).sort(sort).batch_size(2), 2, writes)
    return [(d["_id"], d["z"]) for d in docs]


def keys_sort(cl: Any) -> list:
    return _keys_sort(cl, {"a": {"$gt": 0}}, [("c", 1)])


def keys_sort_of_an_or(cl: Any) -> list:
    return _keys_sort(cl, {"$or": [{"a": {"$gt": 0}}, {"b": 1}]}, [("c", 1)])


def document_sort(cl: Any) -> list:
    return _keys_sort(cl, {"a": {"$gt": 0}}, [("z", 1), ("c", 1)])


def keys_sort_under_a_collation(cl: Any) -> list:
    """The keys of an index without the command's collation, sorted under
    it below the FETCH: equal under strength 2, they keep the scan's order."""
    c = _coll(cl)
    words = ["ab", "Ac", "aB", "b", "AD", "a", "Ae", "abc", "x", "A"]
    c.insert_many([{"_id": i, "w": w} for i, w in enumerate(words)])
    c.create_index("w")

    def writes() -> None:
        c.delete_one({"_id": 0})
        c.insert_one({"_id": 20, "w": "aa"})

    cur = c.find({"w": {"$regex": "^a"}}, collation=COLLATION).sort("w", -1).batch_size(1)
    return _ids(_read(cur, 1, writes))


def id_documents(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": {"a": i}} for i in range(5)])

    def writes() -> None:
        c.insert_one({"_id": {"a": 3.5}})
        c.insert_one({"_id": {"a": 0.5}})
        c.delete_one({"_id": {"a": 4}})

    return _ids(_read(c.find().sort("_id", 1).batch_size(2), 2, writes))


def id_documents_in_a_range(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": {"a": i, "b": [i]}} for i in range(5)])

    def writes() -> None:
        c.insert_one({"_id": {"a": 3.5}})
        c.insert_one({"_id": {"a": 1.5}})

    return _ids(_read(c.find({"_id": {"$gte": {"a": 1}}}).batch_size(2), 2, writes))


def aggregate_under_notablescan(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i} for i in range(40)])
    c.create_index("x")
    cl.admin.command("setParameter", 1, notablescan=True)
    try:
        out = []
        for pipeline in (
            [{"$match": {"x": {"$gte": 0}}}],
            [{"$match": {"x": {"$gte": 0}}}, {"$addFields": {"y": 1}}],
        ):
            cur = c.aggregate(pipeline, batchSize=1)
            first = [next(cur)["_id"]]
            c.delete_many({"x": {"$gte": 30}})
            out.append(first + [d["_id"] for d in cur])
        return out
    finally:
        cl.admin.command("setParameter", 1, notablescan=False)


def aggregate_with_index_key(cl: Any) -> list:
    c = _coll(cl)
    c.insert_many([{"_id": i, "x": i} for i in range(40)])
    c.create_index("x")
    pipeline = [{"$match": {"x": {"$gte": 0}}}, {"$addFields": {"k": {"$meta": "indexKey"}}}]
    cur = c.aggregate(pipeline, batchSize=1)
    first = [next(cur)]
    c.delete_many({})
    return first + list(cur)


SCENARIOS = [
    desc_strings_forward,
    desc_strings_backward,
    desc_compound,
    multikey_forward,
    multikey_backward_sort,
    multikey_shapes,
    multikey_missing_leaf,
    multikey_compound,
    collated_strings,
    collated_strings_descending,
    or_plan,
    or_beside_a_clause,
    or_scanning_the_collection,
    sort_merge_of_an_in,
    sort_merge_of_an_or,
    sort_merge_with_a_filtered_branch,
    min_max,
    min_max_backward,
    min_max_compound_descending,
    return_key,
    return_key_multikey,
    return_key_collection_scan,
    show_record_id,
    keys_sort,
    keys_sort_of_an_or,
    document_sort,
    keys_sort_under_a_collation,
    id_documents,
    id_documents_in_a_range,
    aggregate_under_notablescan,
    aggregate_with_index_key,
]


@pytest.mark.parametrize("scenario", SCENARIOS, ids=[s.__name__ for s in SCENARIOS])
def test_a_cursor_reads_on_as_mongod_does(
    scenario: Callable[[Any], list], rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 returns what its restored plan reaches after the
    writes; the Rust server must return the same, in the same order."""
    same(scenario(rust_mongo), scenario(mongod))
