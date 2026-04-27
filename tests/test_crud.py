from __future__ import annotations

import datetime as dt

import pytest
from bson import ObjectId
from pymongo import MongoClient
from pymongo.errors import DuplicateKeyError

from fongodb import FongoDBServer


@pytest.fixture
def server():
    with FongoDBServer(port=0) as srv:
        yield srv


@pytest.fixture
def client(server: FongoDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


@pytest.fixture
def coll(client: MongoClient):
    return client["testdb"]["things"]


def test_insert_one_then_find(coll) -> None:
    result = coll.insert_one({"name": "alice", "age": 30})
    assert result.acknowledged
    assert result.inserted_id is not None
    found = coll.find_one({"name": "alice"})
    assert found is not None
    assert found["name"] == "alice"
    assert found["age"] == 30
    assert found["_id"] == result.inserted_id


def test_round_trips_objectid_and_datetime(server: FongoDBServer) -> None:
    aware_client = MongoClient(server.uri, tz_aware=True, serverSelectionTimeoutMS=2000)
    try:
        coll = aware_client["testdb"]["things"]
        oid = ObjectId()
        when = dt.datetime(2026, 4, 26, 12, 0, 0, tzinfo=dt.UTC)
        coll.insert_one({"_id": oid, "ts": when})
        doc = coll.find_one({"_id": oid})
        assert doc is not None
        assert doc["_id"] == oid
        assert doc["ts"] == when
    finally:
        aware_client.close()


def test_insert_many_returns_all_ids(coll) -> None:
    docs = [{"i": i} for i in range(5)]
    result = coll.insert_many(docs)
    assert len(result.inserted_ids) == 5
    assert coll.count_documents({}) == 5


def test_count_documents_with_filter(coll) -> None:
    coll.insert_many([{"k": i % 2} for i in range(10)])
    assert coll.count_documents({}) == 10
    assert coll.count_documents({"k": 0}) == 5
    assert coll.count_documents({"k": {"$gte": 1}}) == 5


def test_find_with_dotted_path(coll) -> None:
    coll.insert_many(
        [
            {"name": "a", "addr": {"city": "Dublin"}},
            {"name": "b", "addr": {"city": "Berlin"}},
        ]
    )
    found = list(coll.find({"addr.city": "Dublin"}))
    assert len(found) == 1
    assert found[0]["name"] == "a"


def test_update_one_with_set(coll) -> None:
    coll.insert_one({"_id": 1, "n": 5})
    result = coll.update_one({"_id": 1}, {"$set": {"n": 99}})
    assert result.matched_count == 1
    assert result.modified_count == 1
    assert coll.find_one({"_id": 1})["n"] == 99


def test_update_many_with_inc(coll) -> None:
    coll.insert_many([{"_id": i, "n": 0} for i in range(3)])
    result = coll.update_many({}, {"$inc": {"n": 1}})
    assert result.matched_count == 3
    assert result.modified_count == 3
    assert {d["n"] for d in coll.find()} == {1}


def test_update_with_push(coll) -> None:
    coll.insert_one({"_id": 1, "tags": ["x"]})
    coll.update_one({"_id": 1}, {"$push": {"tags": "y"}})
    assert coll.find_one({"_id": 1})["tags"] == ["x", "y"]


def test_upsert_creates_when_absent(coll) -> None:
    result = coll.update_one({"name": "ghost"}, {"$set": {"seen": True}}, upsert=True)
    assert result.matched_count == 0
    assert result.upserted_id is not None
    found = coll.find_one({"name": "ghost"})
    assert found is not None
    assert found["seen"] is True


def test_replacement_preserves_id(coll) -> None:
    coll.insert_one({"_id": 1, "old": "value"})
    coll.replace_one({"_id": 1}, {"new": "stuff"})
    doc = coll.find_one({"_id": 1})
    assert doc == {"_id": 1, "new": "stuff"}


def test_delete_one(coll) -> None:
    coll.insert_many([{"_id": i, "tag": "x"} for i in range(3)])
    result = coll.delete_one({"tag": "x"})
    assert result.deleted_count == 1
    assert coll.count_documents({}) == 2


def test_delete_many(coll) -> None:
    coll.insert_many([{"_id": i, "tag": "x"} for i in range(3)] + [{"_id": 99, "tag": "y"}])
    result = coll.delete_many({"tag": "x"})
    assert result.deleted_count == 3
    assert coll.count_documents({}) == 1


def test_duplicate_id_raises(coll) -> None:
    coll.insert_one({"_id": 1, "x": 1})
    with pytest.raises(DuplicateKeyError):
        coll.insert_one({"_id": 1, "x": 2})


def test_drop_collection_via_pymongo(client: MongoClient) -> None:
    db = client["dropdb"]
    db["things"].insert_one({"x": 1})
    assert db["things"].count_documents({}) == 1
    db.drop_collection("things")
    assert "things" not in db.list_collection_names()


def test_list_collection_names(client: MongoClient) -> None:
    db = client["lc_db"]
    db["a"].insert_one({"x": 1})
    db["b"].insert_one({"x": 1})
    names = sorted(db.list_collection_names())
    assert names == ["a", "b"]


def test_databases_isolated(client: MongoClient) -> None:
    client["dba"]["c"].insert_one({"_id": 1, "from": "a"})
    client["dbb"]["c"].insert_one({"_id": 1, "from": "b"})
    assert client["dba"]["c"].find_one({"_id": 1})["from"] == "a"
    assert client["dbb"]["c"].find_one({"_id": 1})["from"] == "b"


def test_query_with_in_operator(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    found = list(coll.find({"n": {"$in": [1, 3]}}))
    ids = sorted(d["_id"] for d in found)
    assert ids == [1, 3]


def test_sort_ascending(coll) -> None:
    coll.insert_many([{"n": 3}, {"n": 1}, {"n": 2}])
    result = [d["n"] for d in coll.find().sort("n", 1)]
    assert result == [1, 2, 3]


def test_sort_descending(coll) -> None:
    coll.insert_many([{"n": 3}, {"n": 1}, {"n": 2}])
    result = [d["n"] for d in coll.find().sort("n", -1)]
    assert result == [3, 2, 1]


def test_sort_multi_key(coll) -> None:
    coll.insert_many(
        [
            {"team": "a", "score": 10},
            {"team": "b", "score": 5},
            {"team": "a", "score": 7},
            {"team": "b", "score": 9},
        ]
    )
    result = [(d["team"], d["score"]) for d in coll.find().sort([("team", 1), ("score", -1)])]
    assert result == [("a", 10), ("a", 7), ("b", 9), ("b", 5)]


def test_sort_by_dotted_path(coll) -> None:
    coll.insert_many(
        [
            {"name": "x", "addr": {"zip": "30000"}},
            {"name": "y", "addr": {"zip": "10000"}},
            {"name": "z", "addr": {"zip": "20000"}},
        ]
    )
    result = [d["name"] for d in coll.find().sort("addr.zip", 1)]
    assert result == ["y", "z", "x"]


def test_sort_with_missing_field_sorts_first(coll) -> None:
    coll.insert_many([{"_id": 1, "n": 5}, {"_id": 2}, {"_id": 3, "n": 1}])
    result = [d["_id"] for d in coll.find().sort("n", 1)]
    assert result[0] == 2  # missing sorts first


def test_sort_combined_with_limit(coll) -> None:
    coll.insert_many([{"n": i} for i in range(10)])
    top3 = [d["n"] for d in coll.find().sort("n", -1).limit(3)]
    assert top3 == [9, 8, 7]


def test_projection_inclusion(coll) -> None:
    coll.insert_one({"a": 1, "b": 2, "c": 3})
    doc = coll.find_one({}, {"a": 1, "c": 1})
    assert doc is not None
    assert set(doc.keys()) == {"_id", "a", "c"}


def test_projection_exclude_id(coll) -> None:
    coll.insert_one({"a": 1, "b": 2})
    doc = coll.find_one({}, {"_id": 0, "a": 1})
    assert doc == {"a": 1}


def test_projection_exclusion(coll) -> None:
    coll.insert_one({"_id": 1, "a": 1, "b": 2, "c": 3})
    doc = coll.find_one({}, {"b": 0})
    assert doc == {"_id": 1, "a": 1, "c": 3}


def test_projection_dotted_inclusion(coll) -> None:
    coll.insert_one({"_id": 1, "addr": {"city": "Dublin", "zip": "D02"}, "name": "Joe"})
    doc = coll.find_one({}, {"addr.city": 1, "_id": 0})
    assert doc == {"addr": {"city": "Dublin"}}


def test_small_batch_size_paginates_via_getmore(coll, server: FongoDBServer) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(25)])
    cursor = coll.find().sort("n", 1).batch_size(10)
    docs = list(cursor)
    assert [d["n"] for d in docs] == list(range(25))
    assert len(server.cursors) == 0


def test_iterate_large_collection_completes(coll) -> None:
    coll.insert_many([{"_id": i} for i in range(500)])
    seen = sorted(d["_id"] for d in coll.find())
    assert seen == list(range(500))


def test_close_cursor_kills_it(coll, server: FongoDBServer) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(50)])
    cursor = coll.find().batch_size(5)
    next(cursor)
    assert len(server.cursors) == 1
    cursor.close()
    assert len(server.cursors) == 0


def test_aggregate_paginates_with_small_batch(coll) -> None:
    coll.insert_many([{"_id": i, "g": i % 3} for i in range(60)])
    ids = sorted(d["_id"] for d in coll.aggregate([{"$match": {}}], batchSize=7))
    assert ids == list(range(60))


def test_query_regex_string_form(coll) -> None:
    coll.insert_many([{"name": "alice"}, {"name": "alex"}, {"name": "bob"}])
    names = sorted(d["name"] for d in coll.find({"name": {"$regex": "^al"}}))
    assert names == ["alex", "alice"]


def test_query_regex_compiled_pattern(coll) -> None:
    import re

    coll.insert_many([{"name": "ALICE"}, {"name": "Bob"}])
    found = list(coll.find({"name": re.compile(r"^alice$", re.IGNORECASE)}))
    assert len(found) == 1
    assert found[0]["name"] == "ALICE"


def test_query_type_filter(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "v": "hi"},
            {"_id": 2, "v": 42},
            {"_id": 3, "v": 3.14},
        ]
    )
    string_ids = [d["_id"] for d in coll.find({"v": {"$type": "string"}})]
    assert string_ids == [1]
    number_ids = sorted(d["_id"] for d in coll.find({"v": {"$type": "number"}}))
    assert number_ids == [2, 3]


def test_query_size(coll) -> None:
    coll.insert_many([{"tags": [1]}, {"tags": [1, 2, 3]}, {"tags": [1, 2]}])
    found = list(coll.find({"tags": {"$size": 3}}))
    assert len(found) == 1
    assert found[0]["tags"] == [1, 2, 3]


def test_query_all(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "tags": ["a", "b", "c"]},
            {"_id": 2, "tags": ["a"]},
            {"_id": 3, "tags": ["a", "b"]},
        ]
    )
    ids = sorted(d["_id"] for d in coll.find({"tags": {"$all": ["a", "b"]}}))
    assert ids == [1, 3]


def test_query_mod(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(10)])
    ids = sorted(d["_id"] for d in coll.find({"n": {"$mod": [3, 0]}}))
    assert ids == [0, 3, 6, 9]


def test_find_one_and_update_returns_old_by_default(coll) -> None:
    coll.insert_one({"_id": 1, "n": 5})
    result = coll.find_one_and_update({"_id": 1}, {"$set": {"n": 99}})
    assert result == {"_id": 1, "n": 5}
    assert coll.find_one({"_id": 1})["n"] == 99


def test_find_one_and_update_returns_new_when_requested(coll) -> None:
    from pymongo import ReturnDocument

    coll.insert_one({"_id": 1, "n": 5})
    result = coll.find_one_and_update(
        {"_id": 1}, {"$set": {"n": 99}}, return_document=ReturnDocument.AFTER
    )
    assert result == {"_id": 1, "n": 99}


def test_find_one_and_update_no_match_returns_none(coll) -> None:
    result = coll.find_one_and_update({"_id": 99}, {"$set": {"x": 1}})
    assert result is None


def test_find_one_and_replace_returns_old(coll) -> None:
    coll.insert_one({"_id": 1, "old": "value"})
    result = coll.find_one_and_replace({"_id": 1}, {"new": "stuff"})
    assert result == {"_id": 1, "old": "value"}
    assert coll.find_one({"_id": 1}) == {"_id": 1, "new": "stuff"}


def test_find_one_and_delete_returns_doc(coll) -> None:
    coll.insert_one({"_id": 1, "n": 5})
    result = coll.find_one_and_delete({"_id": 1})
    assert result == {"_id": 1, "n": 5}
    assert coll.count_documents({}) == 0


def test_find_one_and_update_upsert(coll) -> None:
    from pymongo import ReturnDocument

    result = coll.find_one_and_update(
        {"name": "ghost"},
        {"$set": {"seen": True}},
        upsert=True,
        return_document=ReturnDocument.AFTER,
    )
    assert result is not None
    assert result["name"] == "ghost"
    assert result["seen"] is True


def test_find_one_and_update_uses_sort_to_pick(coll) -> None:
    coll.insert_many([{"_id": 1, "n": 10}, {"_id": 2, "n": 5}, {"_id": 3, "n": 20}])
    result = coll.find_one_and_update({}, {"$set": {"picked": True}}, sort=[("n", 1)])
    assert result["_id"] == 2
    picked_ids = sorted(d["_id"] for d in coll.find({"picked": True}))
    assert picked_ids == [2]


def test_find_one_and_update_with_projection(coll) -> None:
    coll.insert_one({"_id": 1, "a": 1, "b": 2, "c": 3})
    result = coll.find_one_and_update({"_id": 1}, {"$set": {"a": 99}}, projection={"a": 1})
    assert result == {"_id": 1, "a": 1}


def test_aggregate_sort_stage(coll) -> None:
    coll.insert_many([{"_id": 1, "n": 3}, {"_id": 2, "n": 1}, {"_id": 3, "n": 2}])
    out = list(coll.aggregate([{"$sort": {"n": 1}}]))
    assert [d["_id"] for d in out] == [2, 3, 1]


def test_aggregate_project_with_computed_field(coll) -> None:
    coll.insert_many([{"_id": 1, "x": 3, "y": 4}])
    out = list(coll.aggregate([{"$project": {"_id": 0, "sum": {"$add": ["$x", "$y"]}}}]))
    assert out == [{"sum": 7}]


def test_aggregate_unwind_stage(coll) -> None:
    coll.insert_one({"_id": 1, "tags": ["a", "b", "c"]})
    out = list(coll.aggregate([{"$unwind": "$tags"}]))
    assert [d["tags"] for d in out] == ["a", "b", "c"]


def test_aggregate_group_with_avg(coll) -> None:
    coll.insert_many(
        [
            {"team": "a", "score": 2},
            {"team": "a", "score": 4},
            {"team": "b", "score": 10},
        ]
    )
    out = sorted(
        coll.aggregate([{"$group": {"_id": "$team", "avg": {"$avg": "$score"}}}]),
        key=lambda d: d["_id"],
    )
    assert out == [{"_id": "a", "avg": 3.0}, {"_id": "b", "avg": 10.0}]


def test_aggregate_replace_root(coll) -> None:
    coll.insert_one({"_id": 1, "inner": {"a": 1, "b": 2}})
    out = list(coll.aggregate([{"$replaceRoot": {"newRoot": "$inner"}}]))
    assert out == [{"a": 1, "b": 2}]


def test_aggregate_chain_unwind_group(coll) -> None:
    coll.insert_many(
        [
            {"team": "a", "scores": [10, 20]},
            {"team": "b", "scores": [5]},
            {"team": "a", "scores": [30]},
        ]
    )
    out = sorted(
        coll.aggregate(
            [
                {"$unwind": "$scores"},
                {"$group": {"_id": "$team", "total": {"$sum": "$scores"}}},
            ]
        ),
        key=lambda d: d["_id"],
    )
    assert out == [{"_id": "a", "total": 60}, {"_id": "b", "total": 5}]


def test_aggregate_addfields(coll) -> None:
    coll.insert_one({"_id": 1, "x": 5})
    out = list(coll.aggregate([{"$addFields": {"doubled": {"$multiply": ["$x", 2]}}}]))
    assert out == [{"_id": 1, "x": 5, "doubled": 10}]


def test_aggregate_lookup_left_outer_join(client: MongoClient) -> None:
    db = client["lookupdb"]
    db["orders"].insert_many(
        [
            {"_id": 1, "item": "abc", "qty": 1},
            {"_id": 2, "item": "xyz", "qty": 5},
            {"_id": 3, "item": "abc", "qty": 10},
        ]
    )
    db["inventory"].insert_many(
        [
            {"_id": "abc", "stock": 100},
            {"_id": "xyz", "stock": 50},
        ]
    )
    pipeline = [
        {
            "$lookup": {
                "from": "inventory",
                "localField": "item",
                "foreignField": "_id",
                "as": "inventory_docs",
            }
        },
        {"$sort": {"_id": 1}},
    ]
    out = list(db["orders"].aggregate(pipeline))
    assert len(out) == 3
    assert out[0]["inventory_docs"] == [{"_id": "abc", "stock": 100}]
    assert out[1]["inventory_docs"] == [{"_id": "xyz", "stock": 50}]
    assert out[2]["inventory_docs"] == [{"_id": "abc", "stock": 100}]


def test_aggregate_lookup_no_match_returns_empty_array(client: MongoClient) -> None:
    db = client["lookupdb2"]
    db["a"].insert_one({"_id": 1, "key": "missing"})
    db["b"].insert_one({"_id": "present", "v": 1})
    out = list(
        db["a"].aggregate(
            [
                {
                    "$lookup": {
                        "from": "b",
                        "localField": "key",
                        "foreignField": "_id",
                        "as": "j",
                    }
                }
            ]
        )
    )
    assert out[0]["j"] == []


def test_query_expr_compares_fields(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "a": 5, "b": 3},
            {"_id": 2, "a": 1, "b": 2},
            {"_id": 3, "a": 7, "b": 7},
        ]
    )
    ids = sorted(d["_id"] for d in coll.find({"$expr": {"$gt": ["$a", "$b"]}}))
    assert ids == [1]


def test_session_can_be_used_for_writes(client: MongoClient) -> None:
    db = client["sessdb"]
    coll = db["things"]
    with client.start_session() as session:
        coll.insert_one({"_id": 1, "x": "a"}, session=session)
        coll.insert_one({"_id": 2, "x": "b"}, session=session)
        docs = list(coll.find({}, session=session).sort("_id", 1))
        assert [d["_id"] for d in docs] == [1, 2]


def test_transaction_context_does_not_error(client: MongoClient) -> None:
    db = client["txndb"]
    coll = db["things"]
    with client.start_session() as session, session.start_transaction():
        coll.insert_one({"_id": 1, "x": "a"}, session=session)
        coll.insert_one({"_id": 2, "x": "b"}, session=session)
    assert coll.count_documents({}) == 2


def test_duplicate_id_int_vs_float_via_pymongo(coll) -> None:
    from pymongo.errors import DuplicateKeyError as PyDup

    coll.insert_one({"_id": 1, "x": "int"})
    with pytest.raises(PyDup):
        coll.insert_one({"_id": 1.0, "x": "float"})
