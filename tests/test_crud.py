from __future__ import annotations

import datetime as dt

import pytest
from bson import ObjectId
from pymongo import MongoClient
from pymongo.errors import DuplicateKeyError

from secantus import SecantusDBServer


@pytest.fixture
def server():
    with SecantusDBServer(port=0) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
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


def test_round_trips_objectid_and_datetime(server: SecantusDBServer) -> None:
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


def test_small_batch_size_paginates_via_getmore(coll, server: SecantusDBServer) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(25)])
    cursor = coll.find().sort("n", 1).batch_size(10)
    docs = list(cursor)
    assert [d["n"] for d in docs] == list(range(25))
    assert len(server.cursors) == 0


def test_iterate_large_collection_completes(coll) -> None:
    coll.insert_many([{"_id": i} for i in range(500)])
    seen = sorted(d["_id"] for d in coll.find())
    assert seen == list(range(500))


def test_close_cursor_kills_it(coll, server: SecantusDBServer) -> None:
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


def test_distinct_simple_field(coll) -> None:
    coll.insert_many([{"team": "a"}, {"team": "b"}, {"team": "a"}, {"team": "c"}])
    teams = sorted(coll.distinct("team"))
    assert teams == ["a", "b", "c"]


def test_distinct_with_filter(coll) -> None:
    coll.insert_many(
        [
            {"team": "a", "active": True},
            {"team": "b", "active": False},
            {"team": "c", "active": True},
        ]
    )
    teams = sorted(coll.distinct("team", {"active": True}))
    assert teams == ["a", "c"]


def test_distinct_unwinds_array_values(coll) -> None:
    coll.insert_many([{"tags": ["a", "b"]}, {"tags": ["b", "c"]}])
    tags = sorted(coll.distinct("tags"))
    assert tags == ["a", "b", "c"]


def test_distinct_dotted_path(coll) -> None:
    coll.insert_many([{"addr": {"city": "Dublin"}}, {"addr": {"city": "Berlin"}}])
    cities = sorted(coll.distinct("addr.city"))
    assert cities == ["Berlin", "Dublin"]


def test_aggregate_with_date_extractors(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "ts": dt.datetime(2026, 1, 15)},
            {"_id": 2, "ts": dt.datetime(2026, 4, 27)},
            {"_id": 3, "ts": dt.datetime(2027, 1, 3)},
        ]
    )
    out = sorted(
        coll.aggregate(
            [
                {"$project": {"_id": 1, "year": {"$year": "$ts"}, "month": {"$month": "$ts"}}},
                {"$sort": {"_id": 1}},
            ]
        ),
        key=lambda d: d["_id"],
    )
    assert out == [
        {"_id": 1, "year": 2026, "month": 1},
        {"_id": 2, "year": 2026, "month": 4},
        {"_id": 3, "year": 2027, "month": 1},
    ]


def test_aggregate_with_array_elem_at(coll) -> None:
    coll.insert_one({"_id": 1, "tags": ["a", "b", "c"]})
    out = list(coll.aggregate([{"$project": {"first_tag": {"$arrayElemAt": ["$tags", 0]}}}]))
    assert out[0]["first_tag"] == "a"


def test_aggregate_with_to_int_conversion(coll) -> None:
    coll.insert_many([{"v": "10"}, {"v": "20"}, {"v": "30"}])
    out = list(
        coll.aggregate(
            [
                {"$project": {"_id": 0, "n": {"$toInt": "$v"}}},
                {"$group": {"_id": None, "total": {"$sum": "$n"}}},
            ]
        )
    )
    assert out == [{"_id": None, "total": 60}]


def test_create_index_listed_via_pymongo(coll) -> None:
    coll.insert_one({"x": 1})
    coll.create_index("x")
    indexes = list(coll.list_indexes())
    names = sorted(i["name"] for i in indexes)
    assert names == ["_id_", "x_1"]


def test_create_index_with_explicit_name_and_compound(coll) -> None:
    coll.insert_one({"a": 1, "b": 2})
    coll.create_index([("a", 1), ("b", -1)], name="ab_idx")
    found = next(i for i in coll.list_indexes() if i["name"] == "ab_idx")
    assert dict(found["key"]) == {"a": 1, "b": -1}


def test_unique_index_blocks_duplicate_insert_via_pymongo(coll) -> None:
    from pymongo.errors import DuplicateKeyError as PyDup

    coll.create_index("email", unique=True)
    coll.insert_one({"email": "alice@example.com"})
    with pytest.raises(PyDup):
        coll.insert_one({"email": "alice@example.com"})


def test_unique_index_blocks_update_via_pymongo(coll) -> None:
    from pymongo.errors import DuplicateKeyError as PyDup

    coll.create_index("email", unique=True)
    coll.insert_many([{"_id": 1, "email": "a@x"}, {"_id": 2, "email": "b@x"}])
    with pytest.raises(PyDup):
        coll.update_one({"_id": 2}, {"$set": {"email": "a@x"}})


def test_drop_index_via_pymongo(coll) -> None:
    coll.insert_one({"x": 1})
    coll.create_index("x")
    coll.drop_index("x_1")
    names = [i["name"] for i in coll.list_indexes()]
    assert names == ["_id_"]


def test_drop_indexes_keeps_id_index(coll) -> None:
    coll.insert_one({"x": 1})
    coll.create_index("x")
    coll.create_index("y")
    coll.drop_indexes()
    names = [i["name"] for i in coll.list_indexes()]
    assert names == ["_id_"]


def test_sparse_unique_index_via_pymongo(coll) -> None:
    coll.create_index("email", unique=True, sparse=True)
    coll.insert_many([{"_id": 1}, {"_id": 2}, {"_id": 3, "email": "x@y"}])
    assert coll.count_documents({}) == 3


def test_query_elem_match_subdoc(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "items": [{"sku": "a", "qty": 1}, {"sku": "b", "qty": 5}]},
            {"_id": 2, "items": [{"sku": "a", "qty": 10}]},
        ]
    )
    found = list(coll.find({"items": {"$elemMatch": {"sku": "a", "qty": {"$gte": 5}}}}))
    assert [d["_id"] for d in found] == [2]


def test_query_elem_match_scalar(coll) -> None:
    coll.insert_many([{"_id": 1, "v": [1, 5, 10]}, {"_id": 2, "v": [1, 2, 3]}])
    found = list(coll.find({"v": {"$elemMatch": {"$gte": 5, "$lt": 11}}}))
    assert [d["_id"] for d in found] == [1]


def test_aggregate_filter_expression(coll) -> None:
    coll.insert_one({"_id": 1, "nums": [1, 2, 3, 4, 5]})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "big": {
                            "$filter": {
                                "input": "$nums",
                                "as": "n",
                                "cond": {"$gte": ["$$n", 3]},
                            }
                        },
                    }
                }
            ]
        )
    )
    assert out == [{"big": [3, 4, 5]}]


def test_aggregate_map_expression(coll) -> None:
    coll.insert_one({"_id": 1, "nums": [1, 2, 3]})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "doubled": {
                            "$map": {
                                "input": "$nums",
                                "as": "n",
                                "in": {"$multiply": ["$$n", 2]},
                            }
                        },
                    }
                }
            ]
        )
    )
    assert out == [{"doubled": [2, 4, 6]}]


def test_aggregate_reduce_expression(coll) -> None:
    coll.insert_one({"_id": 1, "nums": [1, 2, 3, 4]})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "total": {
                            "$reduce": {
                                "input": "$nums",
                                "initialValue": 0,
                                "in": {"$add": ["$$value", "$$this"]},
                            }
                        },
                    }
                }
            ]
        )
    )
    assert out == [{"total": 10}]


def test_projection_elem_match_first_match(coll) -> None:
    coll.insert_one({"_id": 1, "items": [{"qty": 1}, {"qty": 5}, {"qty": 10}]})
    doc = coll.find_one({}, {"items": {"$elemMatch": {"qty": {"$gte": 5}}}})
    assert doc == {"_id": 1, "items": [{"qty": 5}]}


def test_explain_find_returns_query_planner(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    explanation = coll.find({"n": {"$gte": 2}}).explain()
    assert "queryPlanner" in explanation
    assert explanation["queryPlanner"]["namespace"].endswith(".things")
    assert "winningPlan" in explanation["queryPlanner"]
    assert "serverInfo" in explanation


def test_explain_find_collscan_when_no_index(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    plan = coll.find({"n": 2}).explain()["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "COLLSCAN"


def test_explain_find_ixscan_when_indexed(coll) -> None:
    coll.create_index("n")
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    plan = coll.find({"n": 2}).explain()["queryPlanner"]["winningPlan"]
    # MongoDB shape: FETCH wraps an IXSCAN inputStage.
    assert plan["stage"] == "FETCH"
    inner = plan["inputStage"]
    assert inner["stage"] == "IXSCAN"
    assert inner["indexName"] == "n_1"
    assert inner["keyPattern"] == {"n": 1}
    assert inner["direction"] == "forward"


def test_explain_find_ixscan_with_compound_index(coll) -> None:
    coll.create_index([("a", 1), ("b", 1)])
    coll.insert_many([{"_id": i, "a": i, "b": i * 10} for i in range(5)])
    plan = coll.find({"a": 1, "b": 10}).explain()["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "FETCH"
    assert plan["inputStage"]["stage"] == "IXSCAN"
    assert plan["inputStage"]["keyPattern"] == {"a": 1, "b": 1}


def test_explain_find_with_hint_uses_hinted_index(coll) -> None:
    coll.create_index("n")
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    plan = coll.find({"n": 2}).hint("$natural").explain()["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "COLLSCAN"


def test_pipeline_update_via_pymongo(coll) -> None:
    coll.insert_one({"_id": 1, "a": 1, "b": 2})
    coll.update_one({"_id": 1}, [{"$set": {"sum": {"$add": ["$a", "$b"]}}}])
    assert coll.find_one({"_id": 1}) == {"_id": 1, "a": 1, "b": 2, "sum": 3}


def test_gridfs_put_and_get_round_trip(client: MongoClient) -> None:
    import gridfs

    db = client["gfs_db"]
    fs = gridfs.GridFS(db)
    payload = b"hello world from secantus"
    file_id = fs.put(payload, filename="hello.txt", content_type="text/plain")
    out = fs.get(file_id)
    assert out.read() == payload
    assert out.filename == "hello.txt"


def test_gridfs_chunked_payload(client: MongoClient) -> None:
    import gridfs

    db = client["gfs_chunked_db"]
    fs = gridfs.GridFS(db)
    payload = b"x" * (300 * 1024)  # 300 KB, spans multiple default 255 KB chunks
    file_id = fs.put(payload, filename="big.bin")
    out = fs.get(file_id).read()
    assert out == payload


def test_gridfs_delete(client: MongoClient) -> None:
    import gridfs
    from gridfs.errors import NoFile

    db = client["gfs_del_db"]
    fs = gridfs.GridFS(db)
    file_id = fs.put(b"data", filename="x.bin")
    fs.delete(file_id)
    with pytest.raises(NoFile):
        fs.get(file_id)


def test_bulk_write_mixed_ops(coll) -> None:
    from pymongo import DeleteOne, InsertOne, UpdateOne

    coll.insert_one({"_id": 1, "n": 1})
    result = coll.bulk_write(
        [
            InsertOne({"_id": 2, "n": 2}),
            UpdateOne({"_id": 1}, {"$inc": {"n": 10}}),
            DeleteOne({"_id": 2}),
        ]
    )
    assert result.inserted_count == 1
    assert result.modified_count == 1
    assert result.deleted_count == 1
    assert coll.find_one({"_id": 1})["n"] == 11
    assert coll.count_documents({}) == 1


def test_bulk_write_unordered_continues_past_failure(coll) -> None:
    from pymongo import InsertOne
    from pymongo.errors import BulkWriteError

    coll.insert_one({"_id": 1})
    with pytest.raises(BulkWriteError):
        coll.bulk_write(
            [
                InsertOne({"_id": 1}),  # duplicate, should fail
                InsertOne({"_id": 2}),
                InsertOne({"_id": 3}),
            ],
            ordered=False,
        )
    ids = sorted(d["_id"] for d in coll.find())
    assert ids == [1, 2, 3]


def test_rename_collection_via_pymongo(client: MongoClient) -> None:
    db = client["rename_db"]
    db["src"].insert_many([{"_id": 1}, {"_id": 2}])
    db["src"].rename("dst")
    assert "src" not in db.list_collection_names()
    assert "dst" in db.list_collection_names()
    ids = sorted(d["_id"] for d in db["dst"].find())
    assert ids == [1, 2]


def test_server_status_command(client: MongoClient) -> None:
    out = client.admin.command("serverStatus")
    assert out["ok"] == 1.0
    assert "version" in out
    assert "host" in out


def test_db_stats_counts_collections(client: MongoClient) -> None:
    db = client["stats_db"]
    db["a"].insert_many([{"x": 1}, {"x": 2}])
    db["b"].insert_one({"x": 1})
    out = db.command("dbStats")
    assert out["ok"] == 1.0
    assert out["db"] == "stats_db"
    assert out["collections"] >= 2
    assert out["objects"] >= 3


def test_coll_stats_count(client: MongoClient) -> None:
    db = client["coll_stats_db"]
    db["things"].insert_many([{"_id": i} for i in range(5)])
    out = db.command("collStats", "things")
    assert out["ok"] == 1.0
    assert out["count"] == 5


def test_coll_stats_missing_namespace(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    db = client["coll_stats_missing_db"]
    with pytest.raises(OperationFailure):
        db.command("collStats", "missing")


def test_query_with_comment_via_pymongo(coll) -> None:
    coll.insert_many([{"_id": i} for i in range(3)])
    found = list(coll.find({"$comment": "audit-trail"}))
    assert len(found) == 3


def test_aggregate_out_writes_to_collection(client: MongoClient) -> None:
    db = client["out_db"]
    db["src"].insert_many([{"_id": i, "n": i} for i in range(3)])
    list(db["src"].aggregate([{"$match": {"n": {"$gte": 1}}}, {"$out": "dst"}]))
    out = sorted(db["dst"].find(), key=lambda d: d["_id"])
    assert [d["_id"] for d in out] == [1, 2]


def test_aggregate_out_replaces_existing_collection(client: MongoClient) -> None:
    db = client["out_replace_db"]
    db["src"].insert_many([{"_id": 1, "x": "new"}])
    db["dst"].insert_many([{"_id": 99, "x": "old"}])
    list(db["src"].aggregate([{"$out": "dst"}]))
    out = list(db["dst"].find())
    assert len(out) == 1
    assert out[0]["_id"] == 1


def test_aggregate_merge_default_merges_matched(client: MongoClient) -> None:
    db = client["merge_db"]
    db["src"].insert_many([{"_id": 1, "n": 100}, {"_id": 2, "n": 200}])
    db["dst"].insert_many([{"_id": 1, "tag": "old"}, {"_id": 3, "tag": "untouched"}])
    list(db["src"].aggregate([{"$merge": "dst"}]))
    out = sorted(db["dst"].find(), key=lambda d: d["_id"])
    by_id = {d["_id"]: d for d in out}
    assert by_id[1] == {"_id": 1, "n": 100, "tag": "old"}
    assert by_id[2] == {"_id": 2, "n": 200}
    assert by_id[3] == {"_id": 3, "tag": "untouched"}


def test_aggregate_merge_when_matched_replace(client: MongoClient) -> None:
    db = client["merge_replace_db"]
    db["src"].insert_one({"_id": 1, "n": 99})
    db["dst"].insert_one({"_id": 1, "tag": "old"})
    list(db["src"].aggregate([{"$merge": {"into": "dst", "whenMatched": "replace"}}]))
    out = list(db["dst"].find())
    assert out == [{"_id": 1, "n": 99}]


def test_aggregate_merge_when_not_matched_discard(client: MongoClient) -> None:
    db = client["merge_discard_db"]
    db["src"].insert_one({"_id": 1, "n": 99})
    list(db["src"].aggregate([{"$merge": {"into": "dst", "whenNotMatched": "discard"}}]))
    assert list(db["dst"].find()) == []


def test_aggregate_merge_recursively_merges_nested_documents(client: MongoClient) -> None:
    """$merge whenMatched=merge descends into sub-docs rather than overwriting them."""
    db = client["merge_deep_db"]
    db["src"].insert_one({"_id": 1, "addr": {"city": "Dublin", "country": "IE"}})
    db["dst"].insert_one({"_id": 1, "addr": {"street": "Main", "city": "OldCity"}})
    list(db["src"].aggregate([{"$merge": "dst"}]))
    [doc] = list(db["dst"].find())
    # New "city" wins, "country" added, existing "street" preserved.
    assert doc == {"_id": 1, "addr": {"street": "Main", "city": "Dublin", "country": "IE"}}


def test_aggregate_merge_arrays_replace_not_concatenate(client: MongoClient) -> None:
    """Arrays under $merge replace whole, matching real Mongo behaviour."""
    db = client["merge_arr_db"]
    db["src"].insert_one({"_id": 1, "tags": ["a", "b"]})
    db["dst"].insert_one({"_id": 1, "tags": ["x", "y", "z"]})
    list(db["src"].aggregate([{"$merge": "dst"}]))
    [doc] = list(db["dst"].find())
    assert doc == {"_id": 1, "tags": ["a", "b"]}


def test_aggregate_merge_deeply_nested_three_levels(client: MongoClient) -> None:
    db = client["merge_3lvl_db"]
    db["src"].insert_one({"_id": 1, "a": {"b": {"c": 2, "d": 3}}})
    db["dst"].insert_one({"_id": 1, "a": {"b": {"c": 99, "e": 5}, "x": 10}})
    list(db["src"].aggregate([{"$merge": "dst"}]))
    [doc] = list(db["dst"].find())
    assert doc == {"_id": 1, "a": {"b": {"c": 2, "d": 3, "e": 5}, "x": 10}}


def test_aggregate_merge_scalar_overwrites_subdoc(client: MongoClient) -> None:
    """If the new doc replaces a sub-doc with a scalar, scalar wins."""
    db = client["merge_scalar_db"]
    db["src"].insert_one({"_id": 1, "addr": "just a string"})
    db["dst"].insert_one({"_id": 1, "addr": {"street": "Main"}})
    list(db["src"].aggregate([{"$merge": "dst"}]))
    [doc] = list(db["dst"].find())
    assert doc == {"_id": 1, "addr": "just a string"}


def test_positional_all_via_pymongo(coll) -> None:
    coll.insert_one({"_id": 1, "items": [{"qty": 1}, {"qty": 2}, {"qty": 3}]})
    coll.update_one({"_id": 1}, {"$set": {"items.$[].qty": 0}})
    out = coll.find_one({"_id": 1})
    assert [e["qty"] for e in out["items"]] == [0, 0, 0]


def test_positional_filtered_via_pymongo(coll) -> None:
    coll.insert_one({"_id": 1, "items": [{"qty": 1, "tag": "a"}, {"qty": 5, "tag": "b"}]})
    coll.update_one(
        {"_id": 1},
        {"$set": {"items.$[hi].tag": "BIG"}},
        array_filters=[{"hi.qty": {"$gte": 5}}],
    )
    out = coll.find_one({"_id": 1})
    assert out["items"][0]["tag"] == "a"
    assert out["items"][1]["tag"] == "BIG"


def test_positional_dollar_via_pymongo(coll) -> None:
    coll.insert_one({"_id": 1, "items": [{"qty": 1}, {"qty": 5}, {"qty": 9}]})
    coll.update_one({"_id": 1, "items.qty": 5}, {"$set": {"items.$.tag": "MATCH"}})
    out = coll.find_one({"_id": 1})
    assert out["items"][0].get("tag") is None
    assert out["items"][1].get("tag") == "MATCH"
    assert out["items"][2].get("tag") is None


def test_graph_lookup_traverses_chain(client: MongoClient) -> None:
    db = client["graph_db"]
    db["people"].insert_many(
        [
            {"_id": "alice", "reports_to": "bob"},
            {"_id": "bob", "reports_to": "carol"},
            {"_id": "carol", "reports_to": None},
            {"_id": "dave", "reports_to": None},
        ]
    )
    db["start"].insert_one({"_id": 1, "name": "alice"})
    pipeline = [
        {
            "$graphLookup": {
                "from": "people",
                "startWith": "$name",
                "connectFromField": "reports_to",
                "connectToField": "_id",
                "as": "chain",
                "depthField": "depth",
            }
        }
    ]
    out = list(db["start"].aggregate(pipeline))[0]
    chain_ids = sorted((d["_id"], d["depth"]) for d in out["chain"])
    assert chain_ids == [("alice", 0), ("bob", 1), ("carol", 2)]


def test_graph_lookup_max_depth(client: MongoClient) -> None:
    db = client["graph_max_depth_db"]
    db["people"].insert_many(
        [
            {"_id": "a", "next": "b"},
            {"_id": "b", "next": "c"},
            {"_id": "c", "next": "d"},
            {"_id": "d", "next": None},
        ]
    )
    db["start"].insert_one({"_id": 1, "seed": "a"})
    pipeline = [
        {
            "$graphLookup": {
                "from": "people",
                "startWith": "$seed",
                "connectFromField": "next",
                "connectToField": "_id",
                "as": "chain",
                "maxDepth": 1,
            }
        }
    ]
    out = list(db["start"].aggregate(pipeline))[0]
    assert sorted(d["_id"] for d in out["chain"]) == ["a", "b"]


def test_documents_stage(client: MongoClient) -> None:
    db = client["documents_db"]
    db["any"].insert_one({"x": 1})  # need some collection to attach to
    pipeline = [{"$documents": [{"_id": 1, "n": 10}, {"_id": 2, "n": 20}]}]
    out = list(db["any"].aggregate(pipeline))
    assert sorted(d["_id"] for d in out) == [1, 2]


def test_coll_stats_aggregation_stage(client: MongoClient) -> None:
    db = client["collstats_agg_db"]
    db["things"].insert_many([{"_id": i} for i in range(5)])
    out = list(db["things"].aggregate([{"$collStats": {"storageStats": {}}}]))
    assert len(out) == 1
    assert out[0]["ns"] == "collstats_agg_db.things"
    assert out[0]["storageStats"]["count"] == 5


def test_index_stats_aggregation_stage(client: MongoClient) -> None:
    db = client["indexstats_db"]
    db["things"].insert_one({"x": 1})
    db["things"].create_index("x")
    out = list(db["things"].aggregate([{"$indexStats": {}}]))
    names = sorted(d["name"] for d in out)
    assert names == ["_id_", "x_1"]


def test_bucket_auto_via_pymongo(coll) -> None:
    coll.insert_many([{"v": i} for i in range(10)])
    pipeline = [{"$bucketAuto": {"groupBy": "$v", "buckets": 4}}]
    out = list(coll.aggregate(pipeline))
    assert len(out) == 4
    assert sum(b["count"] for b in out) == 10


def test_rename_with_positional_via_pymongo(coll) -> None:
    coll.insert_one({"_id": 1, "items": [{"a": 1}, {"a": 2}]})
    coll.update_one({"_id": 1}, {"$rename": {"items.$[].a": "items.$[].b"}})
    out = coll.find_one({"_id": 1})
    assert out["items"] == [{"b": 1}, {"b": 2}]


def test_lookup_pipeline_form_with_let(client: MongoClient) -> None:
    db = client["lookup_pipe_db"]
    db["orders"].insert_many(
        [
            {"_id": 1, "item": "abc", "qty": 5, "price": 10.0},
            {"_id": 2, "item": "xyz", "qty": 1, "price": 100.0},
        ]
    )
    db["inventory"].insert_many(
        [
            {"_id": "abc", "stock": 100, "min_qty": 1},
            {"_id": "xyz", "stock": 50, "min_qty": 5},
        ]
    )
    pipeline = [
        {
            "$lookup": {
                "from": "inventory",
                "let": {"order_item": "$item", "order_qty": "$qty"},
                "pipeline": [
                    {
                        "$match": {
                            "$expr": {
                                "$and": [
                                    {"$eq": ["$_id", "$$order_item"]},
                                    {"$gte": ["$$order_qty", "$min_qty"]},
                                ]
                            }
                        }
                    }
                ],
                "as": "inv_match",
            }
        },
        {"$sort": {"_id": 1}},
    ]
    out = list(db["orders"].aggregate(pipeline))
    assert len(out) == 2
    assert len(out[0]["inv_match"]) == 1  # abc qty=5 >= min 1
    assert out[0]["inv_match"][0]["_id"] == "abc"
    assert out[1]["inv_match"] == []  # xyz qty=1 < min 5


def test_find_with_hint_uses_named_index(coll) -> None:
    coll.create_index("x")
    coll.insert_many([{"_id": i, "x": i} for i in range(5)])
    docs = list(coll.find({"x": 3}, hint="x_1"))
    assert [d["_id"] for d in docs] == [3]


def test_find_with_hint_by_key_spec(coll) -> None:
    coll.create_index("x")
    coll.insert_many([{"_id": i, "x": i} for i in range(5)])
    docs = list(coll.find({}, hint=[("x", 1)]))
    assert sorted(d["_id"] for d in docs) == [0, 1, 2, 3, 4]


def test_find_with_unknown_hint_returns_bad_value(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"x": 1})
    with pytest.raises(OperationFailure) as exc:
        list(coll.find({}, hint="nonexistent"))
    assert exc.value.code == 2  # BadValue


def test_find_with_natural_hint_is_collection_scan(coll) -> None:
    coll.create_index("x")
    coll.insert_many([{"_id": i, "x": i} for i in range(3)])
    docs = list(coll.find({"x": 1}, hint="$natural"))
    assert [d["_id"] for d in docs] == [1]


def test_aggregate_with_hint(coll) -> None:
    coll.create_index("x")
    coll.insert_many([{"_id": i, "x": i} for i in range(5)])
    docs = list(coll.aggregate([{"$match": {"x": 3}}], hint="x_1"))
    assert [d["_id"] for d in docs] == [3]


def test_aggregate_with_unknown_hint(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"x": 1})
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$match": {}}], hint="nonexistent"))
    assert exc.value.code == 2
