from __future__ import annotations

import contextlib
import datetime as dt

import pymongo
import pytest
from bson import ObjectId
from pymongo import MongoClient
from pymongo.errors import DuplicateKeyError

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    # Real on-disk WiredTiger storage. `tmp_path` is unique per test +
    # parallel worker (xdist), and pytest cleans it up after teardown.
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
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
        when = dt.datetime(2026, 4, 26, 12, 0, 0, tzinfo=dt.timezone.utc)
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


def test_range_operators_are_type_bracketed(coll) -> None:
    """mongod's $gt/$lt/$gte/$lte are type-bracketed: a scalar bound never matches
    a value of a different type bracket. Verified against real mongod (2026-07-13):
    a document-valued field no-matches a numeric bound (rather than erroring), and
    bool is its own bracket — a bool field never matches a numeric bound and vice
    versa, but bool-vs-bool compares (True > False)."""
    coll.insert_many(
        [
            {"_id": 1, "a": 5},
            {"_id": 2, "a": True},
            {"_id": 3, "a": False},
            {"_id": 4, "a": {"x": 1}},
            {"_id": 5, "a": [1, 2, 3]},
            {"_id": 6, "items": [{"k": 1}, {"k": 2}]},
        ]
    )
    # Document-valued field vs a numeric bound: no match, no error.
    assert [d["_id"] for d in coll.find({"a": {"$gt": 2}})] == [1, 5]  # 5, and [1,2,3] via multikey
    # $elemMatch: {$gt: n} over an array of sub-documents: clean no-match.
    assert list(coll.find({"items": {"$elemMatch": {"$gt": 2}}})) == []
    # bool is its own bracket: a bool field never matches a numeric range bound.
    assert [d["_id"] for d in coll.find({"a": {"$lt": 2}})] == [5]  # only [1,2,3] via elem 1<2
    assert [d["_id"] for d in coll.find({"a": {"$gt": 0}})] == [1, 5]
    # bool-vs-bool still compares.
    assert [d["_id"] for d in coll.find({"a": {"$gt": False}})] == [2]
    assert sorted(d["_id"] for d in coll.find({"a": {"$gte": False}})) == [2, 3]


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


def test_inc_and_sum_preserve_int64_over_the_wire(coll) -> None:
    """End-to-end: $inc over an Int64 field and $sum over Int64 values keep the
    BSON 64-bit type through the wire, so a client codec that keys on Int64
    round-trips. Mirrors pymongo's test_custom_types decoder tests."""
    from bson import Int64

    coll.insert_one({"_id": 1, "x": Int64(1)})
    coll.update_one({"_id": 1}, {"$inc": {"x": 1}})
    doc = coll.find_one({"_id": 1})
    assert doc["x"] == 2 and isinstance(doc["x"], Int64)

    coll.insert_many([{"g": "a", "q": Int64(10)}, {"g": "a", "q": Int64(10)}])
    res = list(
        coll.aggregate([{"$match": {"g": "a"}}, {"$group": {"_id": "$g", "t": {"$sum": "$q"}}}])
    )
    assert res[0]["t"] == 20 and isinstance(res[0]["t"], Int64)


def test_inc_mul_on_explicit_null_field_errors(coll) -> None:
    """$inc / $mul on a field present with an explicit null is a TypeMismatch
    (code 14) — mongod refuses to coerce a present non-numeric value to 0. A
    *missing* field is still treated as 0 and the operation applied."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": None})
    with pytest.raises(OperationFailure) as exc_inc:
        coll.update_one({"_id": 1}, {"$inc": {"n": 5}})
    assert exc_inc.value.code == 14

    with pytest.raises(OperationFailure) as exc_mul:
        coll.update_one({"_id": 1}, {"$mul": {"n": 5}})
    assert exc_mul.value.code == 14

    # The null was never coerced to a number.
    assert coll.find_one({"_id": 1})["n"] is None

    # A missing field is still treated as 0 and the delta applied.
    coll.insert_one({"_id": 2})
    coll.update_one({"_id": 2}, {"$inc": {"n": 5}})
    assert coll.find_one({"_id": 2})["n"] == 5


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


def test_duplicate_key_errmsg_matches_mongod_shape(coll) -> None:
    import re

    from pymongo import InsertOne
    from pymongo.errors import BulkWriteError

    coll.insert_one({"_id": 1})
    with pytest.raises(BulkWriteError) as ei:
        coll.bulk_write([InsertOne({"_id": 1})])
    we = ei.value.details["writeErrors"][0]
    # mongod's exact wording: "E11000 duplicate key error collection: <ns>
    # index: <name> dup key: { _id: 1 }". The PHP extension (and other
    # type-strict drivers) pin this message verbatim.
    assert re.fullmatch(
        r"E11000 duplicate key error collection: \w+\.things index: _id_ dup key: \{ _id: 1 \}",
        we["errmsg"],
    ), we["errmsg"]
    assert we["code"] == 11000
    assert we["keyPattern"] == {"_id": 1}
    assert we["keyValue"] == {"_id": 1}


def test_duplicate_key_errmsg_on_unique_index_uses_field_value(coll) -> None:
    import re

    from pymongo import InsertOne
    from pymongo.errors import BulkWriteError

    coll.create_index("email", unique=True)
    coll.insert_one({"_id": 1, "email": "a@b.com"})
    with pytest.raises(BulkWriteError) as ei:
        coll.bulk_write([InsertOne({"_id": 2, "email": "a@b.com"})])
    we = ei.value.details["writeErrors"][0]
    # Non-_id unique index: the dup-key fragment carries the indexed field's
    # value, string-quoted the way the mongo shell prints it.
    assert re.search(r'index: email_1 dup key: \{ email: "a@b\.com" \}', we["errmsg"]), we["errmsg"]


def test_collmod_retunes_ttl_index_expiry(coll) -> None:
    coll.create_index([("lastAccess", 1)], expireAfterSeconds=3)
    res = coll.database.command(
        "collMod",
        coll.name,
        index={"keyPattern": {"lastAccess": 1}, "expireAfterSeconds": 1000},
    )
    # mongod echoes the before/after expiry on a TTL retune.
    assert res["expireAfterSeconds_old"] == 3
    assert res["expireAfterSeconds_new"] == 1000
    # The new value is what listIndexes now reports.
    ix = next(i for i in coll.list_indexes() if i["name"] == "lastAccess_1")
    assert ix["expireAfterSeconds"] == 1000


def test_collmod_by_index_name(coll) -> None:
    coll.create_index([("lastAccess", 1)], expireAfterSeconds=5, name="ttl_idx")
    res = coll.database.command(
        "collMod", coll.name, index={"name": "ttl_idx", "expireAfterSeconds": 60}
    )
    assert res["expireAfterSeconds_old"] == 5
    assert res["expireAfterSeconds_new"] == 60


def test_collmod_unknown_index_errors(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"x": 1})
    with pytest.raises(OperationFailure):
        coll.database.command(
            "collMod", coll.name, index={"keyPattern": {"nope": 1}, "expireAfterSeconds": 10}
        )


def test_server_status_open_cursor_count(coll) -> None:
    def open_cursors() -> int:
        return coll.database.command("serverStatus")["metrics"]["cursor"]["open"]["total"]

    coll.insert_many([{"_id": i} for i in range(3)])
    baseline = open_cursors()

    # A batched find over 3 docs leaves the server cursor alive (1 pending)
    # after the first batch of 2 — the open count rises by exactly one.
    cur = coll.find({}, batch_size=2)
    next(cur)
    assert cur.alive  # 1 doc still pending server-side
    assert open_cursors() == baseline + 1

    # Closing the cursor sends killCursors; the count returns to baseline.
    cur.close()
    assert open_cursors() == baseline


def test_count_hint_sparse_index(coll) -> None:
    coll.insert_many([{"x": 1}, {"x": 2}, {"y": 3}])
    coll.create_index("x", sparse=True, name="sparse_x")
    coll.create_index("y", name="y_1")
    # Hinting the sparse x-index counts only the docs that HAVE x (2 of 3).
    assert coll.count_documents({}, hint="sparse_x") == 2
    assert coll.count_documents({}, hint=[("x", 1)]) == 2
    # A non-sparse index has an entry for every doc (missing field -> null),
    # so it counts them all.
    assert coll.count_documents({}, hint="y_1") == 3


def test_index_info_reports_2dsphere_version(coll) -> None:
    coll.create_index([("pos", "2dsphere")])
    info = next(i for i in coll.list_indexes() if i["name"] == "pos_2dsphere")
    # mongod stamps every 2dsphere index with its format version (>= 3).
    assert info.get("2dsphereIndexVersion", 0) >= 3


def test_find_no_sort_returns_insertion_order(coll) -> None:
    from bson import ObjectId

    # Mixed / non-monotonic _id types where BSON sort order != insertion order.
    docs = [
        {"_id": 1, "x": 11},
        {"_id": ObjectId(), "x": 22},
        {"_id": "foo", "x": 33},
        {"_id": "bar", "x": 44},
    ]
    coll.insert_many(docs)
    # find() with no sort returns insertion order, like mongod — NOT _id order.
    assert [d["x"] for d in coll.find({})] == [11, 22, 33, 44]
    # $natural hint is the same insertion order.
    assert [d["x"] for d in coll.find({}).hint([("$natural", 1)])] == [11, 22, 33, 44]


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


def test_all_argument_validation_via_pymongo(coll) -> None:
    """$all needs an array; mixing $elemMatch with a scalar or using another
    $-op doc is "no $ expressions in $all" (BadValue). mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": [1, 2, 3]})
    for q in (
        {"a": {"$all": 5}},
        {"a": {"$all": [1, {"$elemMatch": {"x": 1}}]}},
        {"a": {"$all": [{"$gt": 1}]}},
    ):
        with pytest.raises(OperationFailure) as exc:
            list(coll.find(q))
        assert exc.value.code == 2, q
    # Valid scalar + all-$elemMatch forms still work.
    assert [d["_id"] for d in coll.find({"a": {"$all": [1, 2]}})] == [1]
    assert [d["_id"] for d in coll.find({"a": {"$all": [{"$elemMatch": {"$gt": 2}}]}})] == [1]


def test_not_elemmatch_validation_via_pymongo(coll) -> None:
    """$not needs a regex or a non-empty document; $elemMatch needs an Object —
    else BadValue. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": 5, "arr": [1, 2, 3]})
    for q in (
        {"a": {"$not": 5}},
        {"a": {"$not": {}}},
        {"a": {"$not": True}},
        {"arr": {"$elemMatch": 5}},
        {"arr": {"$elemMatch": "x"}},
    ):
        with pytest.raises(OperationFailure) as exc:
            list(coll.find(q))
        assert exc.value.code == 2, q
    # Valid forms still work.
    assert [d["_id"] for d in coll.find({"a": {"$not": {"$gt": 9}}})] == [1]
    assert [d["_id"] for d in coll.find({"arr": {"$elemMatch": {"$gt": 2}}})] == [1]


def test_type_argument_validation_via_pymongo(coll) -> None:
    """$type: unknown alias / out-of-range / fractional code -> 2, bool -> 14;
    a whole-double code still matches. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": 5})
    for t, code in [("notatype", 2), (0, 2), (100, 2), (2.5, 2), (True, 14)]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.find({"a": {"$type": t}}))
        assert exc.value.code == code, t
    # Valid: alias, numeric code, and whole-double code.
    assert [d["_id"] for d in coll.find({"a": {"$type": "int"}})] == [1]
    assert [d["_id"] for d in coll.find({"a": {"$type": 16.0}})] == [1]


def test_in_nin_argument_validation_via_pymongo(coll) -> None:
    """$in/$nin need an array (BadValue); a nested $-prefixed doc element is
    rejected ("cannot nest $ under $in"). mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": 5})
    for q in ({"n": {"$in": 5}}, {"n": {"$nin": "x"}}, {"n": {"$in": [{"$x": 1}]}}):
        with pytest.raises(OperationFailure) as exc:
            list(coll.find(q))
        assert exc.value.code == 2, q
    # Valid array (incl. a plain subdoc element) still works.
    assert [d["_id"] for d in coll.find({"n": {"$in": [5, 9]}})] == [1]


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


def test_return_key_replaces_docs_with_index_keys(coll) -> None:
    """``returnKey`` returns only the keys of the index serving the query.
    A find sorted by ``_id`` uses the ``_id`` index, so each result is just
    ``{_id: N}`` — the other fields are stripped. With ``returnKey`` set,
    ``showRecordId`` adds no ``$recordId``. Mirrors pymongo's
    test_command_monitoring 'find with showRecordId and returnKey'."""
    coll.insert_many([{"_id": i, "x": i * 10} for i in range(1, 6)])
    cursor = coll.find({}, sort=[("_id", 1)], show_record_id=True, return_key=True)
    got = list(cursor)
    assert got == [{"_id": i} for i in range(1, 6)]


def test_show_record_id_adds_record_id_field(coll) -> None:
    """``showRecordId`` alone tags each returned doc with a ``$recordId``."""
    coll.insert_many([{"_id": i, "x": i} for i in range(1, 4)])
    docs = list(coll.find({}, sort=[("_id", 1)], show_record_id=True))
    assert all("$recordId" in d for d in docs)
    assert [d["_id"] for d in docs] == [1, 2, 3]
    assert all(d["x"] == d["_id"] for d in docs)  # full doc retained


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


def test_projection_inclusion_exclusion_mix_is_31254(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"a": 1, "b": 2})
    with pytest.raises(OperationFailure) as exc:
        coll.find_one({}, {"a": 1, "b": 0})
    assert exc.value.code == 31254
    assert "Cannot do exclusion on field b in inclusion projection" in str(exc.value)


def test_projection_exclusion_inclusion_mix_is_31253(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"a": 1, "b": 2})
    with pytest.raises(OperationFailure) as exc:
        coll.find_one({}, {"a": 0, "b": 1})
    assert exc.value.code == 31253
    assert "Cannot do inclusion on field b in exclusion projection" in str(exc.value)


def test_projection_dotted_inclusion(coll) -> None:
    coll.insert_one({"_id": 1, "addr": {"city": "Dublin", "zip": "D02"}, "name": "Joe"})
    doc = coll.find_one({}, {"addr.city": 1, "_id": 0})
    assert doc == {"addr": {"city": "Dublin"}}


def test_projection_slice_validation_via_pymongo(coll) -> None:
    """Projection $slice: a non-number scalar / empty / bad array is 28667, a
    2/3-element array that isn't [skip, positive-limit] is 28724. A valid $slice
    still reshapes the array. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": [1, 2, 3, 4, 5]})
    for sl, code in [("x", 28667), ([], 28667), ([1, -2], 28724), ([1, 2, 3], 28724)]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.find({}, {"a": {"$slice": sl}}))
        assert exc.value.code == code, sl
    assert coll.find_one({}, {"_id": 0, "a": {"$slice": 2}})["a"] == [1, 2]
    assert coll.find_one({}, {"_id": 0, "a": {"$slice": [1, 2]}})["a"] == [2, 3]


def test_projection_meta_textscore_without_text_is_40218(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": 1})
    with pytest.raises(OperationFailure) as exc:
        coll.find_one({"a": 1}, {"score": {"$meta": "textScore"}})
    assert exc.value.code == 40218
    assert "query requires text score metadata, but it is not available" in str(exc.value)


def test_projection_meta_unknown_arg_is_17308(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": 1})
    with pytest.raises(OperationFailure) as exc:
        coll.find_one({}, {"score": {"$meta": "bogus"}})
    assert exc.value.code == 17308
    assert "Unsupported argument to $meta: bogus" in str(exc.value)


def test_projection_meta_recognized_arg_omits_field(coll) -> None:
    coll.insert_one({"_id": 1, "a": 1, "b": 2})
    doc = coll.find_one({}, {"m": {"$meta": "indexKey"}})
    # Recognized-but-unsupported $meta arg: field omitted, inclusion keeps _id.
    assert doc == {"_id": 1}


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


def test_regex_options_validation_via_pymongo(coll) -> None:
    """Bad flag -> 51108; non-string $options / $options-without-$regex /
    non-string $regex -> BadValue. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "s": "Hello"})
    with pytest.raises(OperationFailure) as exc:
        list(coll.find({"s": {"$regex": "h", "$options": "z"}}))
    assert exc.value.code == 51108
    for q in ({"s": {"$options": "i"}}, {"s": {"$regex": 5}}):
        with pytest.raises(OperationFailure) as exc:
            list(coll.find(q))
        assert exc.value.code == 2, q
    # A valid case-insensitive regex still matches.
    assert [d["_id"] for d in coll.find({"s": {"$regex": "^h", "$options": "i"}})] == [1]


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


def test_query_all_scalar_field(coll) -> None:
    """$all against a scalar field value matches like a one-element array
    (mongod 7.0.12); an empty $all matches nothing. Regression for a bug where
    both servers silently missed scalar-field docs."""
    from bson import Regex

    coll.insert_many(
        [
            {"_id": 1, "tags": ["red", "blue"]},
            {"_id": 2, "tags": "red"},  # scalar field
            {"_id": 3, "tags": "green"},
        ]
    )
    assert sorted(d["_id"] for d in coll.find({"tags": {"$all": ["red"]}})) == [1, 2]
    assert sorted(d["_id"] for d in coll.find({"tags": {"$all": [Regex("^red$")]}})) == [1, 2]
    assert list(coll.find({"tags": {"$all": []}})) == []


def test_query_all_with_elemmatch(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "a": [1, 2, 3]},
            {"_id": 2, "a": [4, 5]},
            {"_id": 3, "a": [1, 5, 10]},
        ]
    )
    ids = sorted(
        d["_id"] for d in coll.find({"a": {"$all": [{"$elemMatch": {"$gt": 1, "$lt": 3}}]}})
    )
    assert ids == [1]
    ids2 = sorted(
        d["_id"]
        for d in coll.find(
            {"a": {"$all": [{"$elemMatch": {"$gt": 4}}, {"$elemMatch": {"$lt": 2}}]}}
        )
    )
    assert ids2 == [3]


def test_query_in_with_regex(coll) -> None:
    from bson import Regex

    coll.insert_many(
        [
            {"_id": 1, "s": "hello"},
            {"_id": 2, "s": "World"},
            {"_id": 3, "s": "HELLO"},
            {"_id": 4, "s": "hi"},
        ]
    )
    ids = sorted(d["_id"] for d in coll.find({"s": {"$in": [Regex("^h", "i")]}}))
    assert ids == [1, 3, 4]
    # $nin excludes the regex matches.
    ids2 = sorted(d["_id"] for d in coll.find({"s": {"$nin": [Regex("^h", "i")]}}))
    assert ids2 == [2]


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


def test_aggregate_sort_stage_validation_via_pymongo(coll) -> None:
    """$sort: non-numeric direction 15974, numeric non-±1 15975, empty spec 15976.
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": 1, "n": 3}, {"_id": 2, "n": 1}])
    for spec, code in [
        ({"n": "asc"}, 15974),
        ({"n": True}, 15974),
        ({"n": 0}, 15975),
        ({"n": 2}, 15975),
        ({}, 15976),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$sort": spec}]))
        assert exc.value.code == code, spec
    # A whole-double direction still sorts.
    assert [d["_id"] for d in coll.aggregate([{"$sort": {"n": 1.0}}])] == [2, 1]


def test_facet_validation_via_pymongo(coll) -> None:
    """$facet: empty/non-object spec 40169, non-array sub-pipeline 40170,
    non-object stage 40171, nested $facet 40600. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
    for spec, code in [
        ({}, 40169),
        ({"a": 5}, 40170),
        ({"a": [5]}, 40171),
        ({"a": [{"$facet": {"b": [{"$match": {"v": 1}}]}}]}, 40600),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$facet": spec}]))
        assert exc.value.code == code, spec
    # A valid $facet still runs its sub-pipelines.
    out = list(coll.aggregate([{"$facet": {"n": [{"$count": "c"}]}}]))
    assert out == [{"n": [{"c": 2}]}]


def test_count_stage_validation_via_pymongo(coll) -> None:
    """$count: non-string 40156, empty 40157, $-prefixed 40158, dotted 40160,
    "_id" 15948. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": 1}, {"_id": 2}, {"_id": 3}])
    for spec, code in [
        (5, 40156),
        ("", 40157),
        ("$n", 40158),
        ("a.b", 40160),
        ("_id", 15948),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$count": spec}]))
        assert exc.value.code == code, spec
    assert list(coll.aggregate([{"$count": "n"}])) == [{"n": 3}]


def test_project_empty_spec_via_pymongo(coll) -> None:
    """An empty $project spec is Location51272. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "v": 1})
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {}}]))
    assert exc.value.code == 51272


def test_sort_by_count_validation_via_pymongo(coll) -> None:
    """$sortByCount: number/bool/array/null 40149, bare string 40148, non-`$`
    object 40147. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 1}, {"_id": 3, "v": 2}])
    for spec, code in [
        (5, 40149),
        (True, 40149),
        ([1], 40149),
        (None, 40149),
        ("v", 40148),
        ({"a": 1}, 40147),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$sortByCount": spec}]))
        assert exc.value.code == code, spec
    # A $-prefixed path and a single-op expression object are both valid.
    assert list(coll.aggregate([{"$sortByCount": "$v"}])) == [
        {"_id": 1, "count": 2},
        {"_id": 2, "count": 1},
    ]


def test_bucket_auto_validation_via_pymongo(coll) -> None:
    """$bucketAuto: non-numeric/bool buckets 40241, fractional 40242, non-positive
    40243, missing groupBy/buckets 40246; a whole-double buckets computes.
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": i, "v": i} for i in range(6)])
    for spec, code in [
        ({"groupBy": "$v", "buckets": True}, 40241),
        ({"groupBy": "$v", "buckets": "x"}, 40241),
        ({"groupBy": "$v", "buckets": 2.5}, 40242),
        ({"groupBy": "$v", "buckets": 0}, 40243),
        ({"groupBy": "$v", "buckets": -1}, 40243),
        ({"groupBy": "$v"}, 40246),
        ({"buckets": 2}, 40246),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$bucketAuto": spec}]))
        assert exc.value.code == code, spec
    # A whole-double buckets is accepted.
    out = list(coll.aggregate([{"$bucketAuto": {"groupBy": "$v", "buckets": 2.0}}]))
    assert len(out) == 2
    # granularity name validation: non-string -> 40261, unknown -> 40257.
    for gran, code in [(5, 40261), ("BOGUS", 40257)]:
        with pytest.raises(OperationFailure) as exc:
            list(
                coll.aggregate(
                    [{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": gran}}]
                )
            )
        assert exc.value.code == code, gran


def test_bucket_auto_granularity_via_pymongo(coll) -> None:
    """$bucketAuto `granularity` preferred-number rounding over the wire: exact
    boundaries (incl. mongod's non-standard ULP 63*0.1 = 6.300000000000001) and
    the value-error codes. Boundaries verified hex-exact against mongod 7.0.12."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": i, "v": i + 1} for i in range(8)])  # v = 1..8
    out = list(
        coll.aggregate([{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": "R5"}}])
    )
    assert [(b["_id"]["min"], b["_id"]["max"], b["count"]) for b in out] == [
        (0.63, 6.300000000000001, 6),
        (6.300000000000001, 10.0, 2),
    ]
    out = list(
        coll.aggregate(
            [{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": "POWERSOF2"}}]
        )
    )
    assert [(b["_id"]["min"], b["_id"]["max"]) for b in out] == [(0.5, 8.0), (8.0, 16.0)]

    # value must be a non-negative number: non-numeric 40258, NaN 40259, negative 40260
    for values, code in [
        ([-1.0, 2.0, 3.0], 40260),
        ([1.0, 2.0, "s"], 40258),
        ([float("nan"), 1.0], 40259),
    ]:
        coll.delete_many({})
        coll.insert_many([{"_id": i, "v": v} for i, v in enumerate(values)])
        with pytest.raises(OperationFailure) as exc:
            list(
                coll.aggregate(
                    [{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": "R5"}}]
                )
            )
        assert exc.value.code == code, values


def test_projection_elem_match_non_document_via_pymongo(coll) -> None:
    """A non-document $elemMatch projection argument is Location31274.
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "arr": [1, 2, 3]})
    for arg in (5, "x", [1]):
        with pytest.raises(OperationFailure) as exc:
            list(coll.find({}, {"arr": {"$elemMatch": arg}}))
        assert exc.value.code == 31274, arg


def test_pull_pullall_non_array_via_pymongo(coll) -> None:
    """$pull / $pullAll on a present but non-array field is code 2; a missing
    field is a silent no-op. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "num": 5, "nul": None, "arr": [1, 2, 3]})
    for upd in ({"$pull": {"num": 1}}, {"$pullAll": {"num": [1]}}):
        with pytest.raises(OperationFailure) as exc:
            coll.update_one({"_id": 1}, upd)
        assert exc.value.code == 2, upd
    for upd in ({"$pull": {"nul": 1}}, {"$pullAll": {"nul": [1]}}):
        with pytest.raises(OperationFailure) as exc:
            coll.update_one({"_id": 1}, upd)
        assert exc.value.code == 2, upd
    # Missing field: no-op. Valid array pull still works.
    coll.update_one({"_id": 1}, {"$pull": {"nope": 1}})
    coll.update_one({"_id": 1}, {"$pull": {"arr": 2}})
    assert coll.find_one({"_id": 1})["arr"] == [1, 3]


def test_push_sort_validation_via_pymongo(coll) -> None:
    """$push $sort: a numeric whole-element sort must be ±1, a document direction
    must be ±1, and a non-numeric spec is code 2; a whole-double ±1 is accepted.
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": [{"s": 3}, {"s": 1}]})
    for spec in (2, 1.5, "x", True, {"s": 2}, {"s": True}):
        with pytest.raises(OperationFailure) as exc:
            coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [{"s": 2}], "$sort": spec}}})
        assert exc.value.code == 2, spec
    # A whole-double ±1 direction is accepted and sorts.
    coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [{"s": 2}], "$sort": {"s": 1.0}}}})
    assert [e["s"] for e in coll.find_one({"_id": 1})["a"]] == [1, 2, 3]


def test_current_date_bool_false_via_pymongo(coll) -> None:
    """$currentDate accepts a boolean (true OR false) as the set-Date form, and a
    {$type} object; a non-bool scalar / bad $type is code 2. mongod 7.0.12-verified."""
    import datetime

    from bson import Timestamp
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    coll.update_one({"_id": 1}, {"$currentDate": {"d": False}})
    assert isinstance(coll.find_one({"_id": 1})["d"], datetime.datetime)
    coll.update_one({"_id": 1}, {"$currentDate": {"ts": {"$type": "timestamp"}}})
    assert isinstance(coll.find_one({"_id": 1})["ts"], Timestamp)
    for opt in (5, "x", {"$type": "bogus"}):
        with pytest.raises(OperationFailure) as exc:
            coll.update_one({"_id": 1}, {"$currentDate": {"bad": opt}})
        assert exc.value.code == 2, opt


def test_array_filters_validation_via_pymongo(coll) -> None:
    """arrayFilters: a non-object filter (14), an empty filter (9), a bad
    identifier (2), a duplicate identifier (9), and an unused identifier (9) are
    rejected; a valid filter updates the matching elements. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure, WriteError

    coll.insert_one({"_id": 1, "a": [{"g": 1}, {"g": 5}]})
    upd = {"$set": {"a.$[x].g": 9}}
    for af, code in [
        (["x"], 14),
        ([{}], 9),
        ([{"1x": {"$gt": 0}}], 2),
        ([{"X": {"$gt": 0}}], 2),
        ([{"x": {"$gt": 0}}, {"x": {"$lt": 9}}], 9),
        ([{"x": {"$gt": 0}}, {"y": {"$gt": 0}}], 9),
        ([{"x": {"$gt": 0}, "y": {"$gt": 0}}], 9),  # two identifiers in one filter
        ([{"$and": [{"x": {"$gt": 0}}, {"y": {"$gt": 0}}]}], 9),  # two, nested
        ([{"$expr": {"$gt": ["$g", 0]}}], 224),  # $expr, no identifier
    ]:
        with pytest.raises((OperationFailure, WriteError)) as exc:
            coll.update_one({"_id": 1}, upd, array_filters=af)
        assert exc.value.code == code, af
    # A valid filter updates the matching elements.
    coll.update_one({"_id": 1}, upd, array_filters=[{"x.g": {"$gt": 3}}])
    assert [e["g"] for e in coll.find_one({"_id": 1})["a"]] == [1, 9]
    # A single identifier nested inside $and resolves and applies to the match.
    coll.update_one(
        {"_id": 1}, {"$set": {"a.$[x].g": 7}}, array_filters=[{"$and": [{"x.g": {"$gt": 8}}]}]
    )
    assert [e["g"] for e in coll.find_one({"_id": 1})["a"]] == [1, 7]


def test_densify_validation_via_pymongo(coll) -> None:
    """$densify: date unit on numeric 6053600, bool step 14, non-positive step
    5733401, bad bounds string 5946802, wrong-length array 5733403, descending
    array 5733402. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 5}])
    for rng, code in [
        ({"step": 1, "unit": "day", "bounds": "full"}, 6053600),
        ({"step": True, "bounds": "full"}, 14),
        ({"step": 0, "bounds": "full"}, 5733401),
        ({"step": 1, "bounds": "partial"}, 5946802),
        ({"step": 1, "bounds": [0]}, 5733403),
        ({"step": 1, "bounds": [5, 0]}, 5733402),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$densify": {"field": "v", "range": rng}}]))
        assert exc.value.code == code, rng
    # A valid numeric densify still fills the gap.
    out = list(
        coll.aggregate([{"$densify": {"field": "v", "range": {"step": 1, "bounds": "full"}}}])
    )
    assert sorted(d["v"] for d in out) == [1, 2, 3, 4, 5]


def test_aggregate_project_with_computed_field(coll) -> None:
    coll.insert_many([{"_id": 1, "x": 3, "y": 4}])
    out = list(coll.aggregate([{"$project": {"_id": 0, "sum": {"$add": ["$x", "$y"]}}}]))
    assert out == [{"sum": 7}]


def test_aggregate_getfield_absent_is_omitted(coll) -> None:
    # $getField reading a field absent from the input resolves to "missing";
    # a $project computed field that resolves to missing is OMITTED entirely
    # (matching mongod) — never emitted as null. But an input that resolves to
    # an explicit null yields null (the field is emitted).
    coll.insert_many(
        [
            {"_id": 1, "sub": {"k": 1}},
            {"_id": 2, "sub": {"j": 2}},
            {"_id": 3, "sub": None},
            {"_id": 5},
        ]
    )
    out = list(
        coll.aggregate(
            [
                {"$sort": {"_id": 1}},
                {"$project": {"r": {"$getField": {"field": "k", "input": "$sub"}}}},
            ]
        )
    )
    # _id:1 -> r=1; _id:2 (no k) omitted; _id:3 (sub null) -> r=null; _id:5 (no sub) omitted.
    assert out == [{"_id": 1, "r": 1}, {"_id": 2}, {"_id": 3, "r": None}, {"_id": 5}]


def test_aggregate_getfield_present_null_is_emitted(coll) -> None:
    # A field present with an explicit null returns null and IS emitted.
    coll.insert_one({"_id": 1, "sub": {"k": None}})
    pipeline = [{"$project": {"r": {"$getField": {"field": "k", "input": "$sub"}}}}]
    out = list(coll.aggregate(pipeline))
    assert out == [{"_id": 1, "r": None}]


def test_aggregate_new_expression_operators(coll) -> None:
    from bson import Timestamp

    coll.insert_one({"_id": 1, "ts": Timestamp(1700000000, 7), "n": 5, "arr": [1], "s": "Hello"})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "tsSec": {"$tsSecond": "$ts"},
                        "tsInc": {"$tsIncrement": "$ts"},
                        "type_n": {"$type": "$n"},
                        "type_miss": {"$type": "$nope"},
                        "isNum": {"$isNumber": "$n"},
                        "isArr": {"$isArray": "$arr"},
                        "scc": {"$strcasecmp": ["$s", "HELLO"]},
                        "rep": {
                            "$replaceAll": {"input": "abcabc", "find": "bc", "replacement": "X"}
                        },
                        "iso": {
                            "$dateFromParts": {"isoWeekYear": 2023, "isoWeek": 5, "isoDayOfWeek": 3}
                        },
                    }
                }
            ]
        )
    )
    assert out == [
        {
            "tsSec": 1700000000,
            "tsInc": 7,
            "type_n": "int",
            "type_miss": "missing",
            "isNum": True,
            "isArr": True,
            "scc": 0,
            "rep": "aXaX",
            "iso": dt.datetime(2023, 2, 1),
        }
    ]


def test_aggregate_set_operators(coll) -> None:
    coll.insert_one({"_id": 1, "a": 5, "b": "x"})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "union": {"$setUnion": [[3, 1, 2], [5, 4]]},
                        "inter": {"$setIntersection": [[3, 1, 2, 5], [2, 5, 1]]},
                        "diff": {"$setDifference": [[5, 3, 1, 2], [3]]},
                        "eq": {"$setEquals": [[1, 2], [2, 1]]},
                        "sub": {"$setIsSubset": [[1, 2], [1, 2, 3]]},
                        "allt": {"$allElementsTrue": [[1, True]]},
                        "cmp": {"$cmp": [1, 2]},
                        "bsz": {"$bsonSize": "$$ROOT"},
                    }
                }
            ]
        )
    )
    assert out == [
        {
            "union": [1, 2, 3, 4, 5],
            "inter": [1, 2, 5],
            "diff": [5, 1, 2],
            "eq": True,
            "sub": True,
            "allt": True,
            "cmp": -1,
            "bsz": 30,
        }
    ]


def test_aggregate_trig_operators(coll) -> None:
    import math

    coll.insert_one({"_id": 1})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "sin": {"$sin": 0},
                        "cos": {"$cos": 0},
                        "tan": {"$tan": 0},
                        "asin": {"$asin": 1},
                        "acos": {"$acos": 1},
                        "atan": {"$atan": 1},
                        "atan2": {"$atan2": [1, 1]},
                        "sinh": {"$sinh": 0},
                        "cosh": {"$cosh": 0},
                        "tanh": {"$tanh": 0},
                        "asinh": {"$asinh": 0},
                        "acosh": {"$acosh": 1},
                        "atanh": {"$atanh": 0},
                        "atanh_edge": {"$atanh": 1},
                        "null": {"$sin": None},
                    }
                }
            ]
        )
    )
    assert out == [
        {
            "sin": 0.0,
            "cos": 1.0,
            "tan": 0.0,
            "asin": math.pi / 2,
            "acos": 0.0,
            "atan": math.pi / 4,
            "atan2": math.pi / 4,
            "sinh": 0.0,
            "cosh": 1.0,
            "tanh": 0.0,
            "asinh": 0.0,
            "acosh": 0.0,
            "atanh": 0.0,
            "atanh_edge": math.inf,
            "null": None,
        }
    ]
    # Domain error surfaces (mongod Location50989 on the Python server).
    with pytest.raises(pymongo.errors.OperationFailure):
        list(coll.aggregate([{"$project": {"r": {"$asin": 5}}}]))


def test_aggregate_date_from_parts(coll) -> None:
    coll.insert_one({"_id": 1})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "basic": {"$dateFromParts": {"year": 2023, "month": 6, "day": 15}},
                        "rollover": {"$dateFromParts": {"year": 2023, "month": 13, "day": 1}},
                        "tz": {
                            "$dateFromParts": {
                                "year": 2023,
                                "month": 6,
                                "day": 15,
                                "hour": 12,
                                "timezone": "+05:00",
                            }
                        },
                    }
                }
            ]
        )
    )
    # pymongo returns naive UTC datetimes by default.
    assert out == [
        {
            "basic": dt.datetime(2023, 6, 15),
            "rollover": dt.datetime(2024, 1, 1),
            "tz": dt.datetime(2023, 6, 15, 7, 0),  # 12:00 +05:00 -> 07:00 UTC
        }
    ]


def test_aggregate_to_date(coll) -> None:
    # $toDate: <expr> is shorthand for $convert: {input: <expr>, to: "date"}.
    # A date is returned unchanged; an int/long/double is milliseconds since the
    # Unix epoch; an ISO string is parsed; null / missing -> null. (ObjectId is
    # NOT converted — SecantusDB's $convert-to-date, which $toDate delegates to,
    # doesn't yet support that source, so $toDate mirrors it exactly.)
    stored = dt.datetime(2020, 5, 6, 7, 8, 9)
    coll.insert_one({"_id": 1, "d": stored, "ms": 1700000000000, "s": "2026-04-28T12:00:00"})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "from_date": {"$toDate": "$d"},
                        "from_millis": {"$toDate": "$ms"},
                        "from_string": {"$toDate": "$s"},
                        "from_null": {"$toDate": None},
                        "from_missing": {"$toDate": "$nope"},
                    }
                }
            ]
        )
    )
    # pymongo returns naive UTC datetimes by default.
    assert out == [
        {
            "from_date": stored,
            "from_millis": dt.datetime(2023, 11, 14, 22, 13, 20),
            "from_string": dt.datetime(2026, 4, 28, 12, 0, 0),
            "from_null": None,
            "from_missing": None,
        }
    ]


def test_date_arg_validation_via_pymongo(coll) -> None:
    """$dateAdd/$dateSubtract amount and $dateTrunc binSize: whole double accepted,
    fractional/bool -> 5166405 / 5439017, non-positive binSize -> 5439018. mongod
    7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "d": dt.datetime(2021, 1, 1)})
    for op in ("$dateAdd", "$dateSubtract"):
        for bad in (2.5, True):
            with pytest.raises(OperationFailure) as exc:
                list(
                    coll.aggregate(
                        [
                            {
                                "$project": {
                                    "r": {op: {"startDate": "$d", "unit": "day", "amount": bad}}
                                }
                            }
                        ]
                    )
                )
            assert exc.value.code == 5166405, (op, bad)
    for bad, code in [(2.5, 5439017), (True, 5439017), (-1, 5439018)]:
        with pytest.raises(OperationFailure) as exc:
            list(
                coll.aggregate(
                    [
                        {
                            "$project": {
                                "r": {"$dateTrunc": {"date": "$d", "unit": "day", "binSize": bad}}
                            }
                        }
                    ]
                )
            )
        assert exc.value.code == code, bad
    # A whole-double amount still computes.
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "r": {"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 2.0}},
                    }
                }
            ]
        )
    )
    assert out == [{"r": dt.datetime(2021, 1, 3)}]


def test_to_date_rejects_bool_via_pymongo(coll) -> None:
    """$toDate: a bool is ConversionFailure (241), not coerced to a date; $convert
    with onError catches it. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {"r": {"$toDate": True}, "_id": 0}}]))
    assert exc.value.code == 241
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "r": {"$convert": {"input": True, "to": "date", "onError": "x"}},
                    }
                }
            ]
        )
    )
    assert out == [{"r": "x"}]


def test_aggregate_date_extractor_timezone(coll) -> None:
    # 2023-01-15T16:30Z: UTC hour 16; America/New_York is EST (-05:00) -> hour 11,
    # still the 15th. The {date, timezone} object form is mongod's timezone-aware
    # extractor spec; a bare "$d" reads UTC.
    coll.insert_one({"_id": 1, "d": dt.datetime(2023, 1, 15, 16, 30, tzinfo=dt.timezone.utc)})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "utc_hour": {"$hour": "$d"},
                        "ny_hour": {"$hour": {"date": "$d", "timezone": "America/New_York"}},
                        "ny_day": {"$dayOfMonth": {"date": "$d", "timezone": "America/New_York"}},
                        "off_hour": {"$hour": {"date": "$d", "timezone": "+05:30"}},
                    }
                }
            ]
        )
    )
    assert out == [{"utc_hour": 16, "ny_hour": 11, "ny_day": 15, "off_hour": 22}]


def test_aggregate_date_to_parts_timezone(coll) -> None:
    # $dateToParts reads local wall-clock in the given zone. 16:30:45Z is EST
    # (-05:00) in New York -> hour 11, still the 15th.
    coll.insert_one({"_id": 1, "d": dt.datetime(2023, 1, 15, 16, 30, 45, tzinfo=dt.timezone.utc)})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "parts": {"$dateToParts": {"date": "$d", "timezone": "America/New_York"}},
                    }
                }
            ]
        )
    )
    assert out[0]["parts"] == {
        "year": 2023,
        "month": 1,
        "day": 15,
        "hour": 11,
        "minute": 30,
        "second": 45,
        "millisecond": 0,
    }


def test_aggregate_date_component_extractors(coll) -> None:
    # 2027-01-01 is a Friday belonging to ISO year 2026 week 53; also exercise a
    # timezone-shifted day-boundary crossing and the millisecond component.
    coll.insert_one(
        {"_id": 1, "d": dt.datetime(2026, 3, 15, 10, 30, 45, 123000, tzinfo=dt.timezone.utc)}
    )
    coll.insert_one({"_id": 2, "d": dt.datetime(2027, 1, 1, tzinfo=dt.timezone.utc)})
    # 2026-03-15T02:00Z -> -05:00 is the 14th (day of year 73).
    coll.insert_one({"_id": 3, "d": dt.datetime(2026, 3, 15, 2, 0, tzinfo=dt.timezone.utc)})
    out = list(
        coll.aggregate(
            [
                {"$sort": {"_id": 1}},
                {
                    "$project": {
                        "_id": 1,
                        "doy": {"$dayOfYear": "$d"},
                        "week": {"$week": "$d"},
                        "isoweek": {"$isoWeek": "$d"},
                        "isodow": {"$isoDayOfWeek": "$d"},
                        "isoyear": {"$isoWeekYear": "$d"},
                        "ms": {"$millisecond": "$d"},
                        "tz_doy": {"$dayOfYear": {"date": "$d", "timezone": "-05:00"}},
                    }
                },
            ]
        )
    )
    assert out[0] == {
        "_id": 1,
        "doy": 74,
        "week": 11,
        "isoweek": 11,
        "isodow": 7,  # Sunday
        "isoyear": 2026,
        "ms": 123,
        "tz_doy": 74,
    }
    assert out[1]["isoweek"] == 53
    assert out[1]["isoyear"] == 2026
    assert out[1]["isodow"] == 5  # Friday
    assert out[2]["tz_doy"] == 73  # shifted to the 14th (day-of-year 73)


def test_aggregate_date_extractor_non_date_errors(coll) -> None:
    """A date extractor on a non-date value errors (mongod Location16006); null
    and a missing field yield null."""
    coll.insert_one({"_id": 1, "s": "not a date", "z": None})
    for op in ("$year", "$dayOfYear", "$isoWeek", "$millisecond"):
        with pytest.raises(pymongo.errors.OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": {op: "$s"}}}]))
        assert exc.value.code == 16006
        # null / missing -> null (not an error).
        out = list(coll.aggregate([{"$project": {"_id": 0, "r": {op: "$z"}}}]))
        assert out == [{"r": None}]
        out2 = list(coll.aggregate([{"$project": {"_id": 0, "r": {op: "$nope"}}}]))
        assert out2 == [{"r": None}]


def test_aggregate_date_to_parts_iso8601(coll) -> None:
    # 2027-01-01 (Friday) belongs to ISO year 2026, week 53.
    coll.insert_one({"_id": 1, "d": dt.datetime(2027, 1, 1, 13, 14, 15, 678000)})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "iso": {"$dateToParts": {"date": "$d", "iso8601": True}},
                        "civil": {"$dateToParts": {"date": "$d", "iso8601": False}},
                    }
                }
            ]
        )
    )
    assert out[0]["iso"] == {
        "isoWeekYear": 2026,
        "isoWeek": 53,
        "isoDayOfWeek": 5,
        "hour": 13,
        "minute": 14,
        "second": 15,
        "millisecond": 678,
    }
    assert out[0]["civil"] == {
        "year": 2027,
        "month": 1,
        "day": 1,
        "hour": 13,
        "minute": 14,
        "second": 15,
        "millisecond": 678,
    }


def test_aggregate_max_n_min_n(coll) -> None:
    coll.insert_one({"_id": 1, "a": [3, 1, 4, 1, 5, 9, 2, 6], "with_nulls": [3, None, 1, None, 5]})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "top3": {"$maxN": {"n": 3, "input": "$a"}},
                        "bot3": {"$minN": {"n": 3, "input": "$a"}},
                        "max_nn": {"$maxN": {"n": 2, "input": "$with_nulls"}},
                    }
                }
            ]
        )
    )
    assert out == [{"top3": [9, 6, 5], "bot3": [1, 1, 2], "max_nn": [5, 3]}]


def test_aggregate_first_n_last_n(coll) -> None:
    coll.insert_one({"_id": 1, "a": [10, 20, 30, 40, 50]})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "f": {"$firstN": {"n": 2, "input": "$a"}},
                        "l": {"$lastN": {"n": 2, "input": "$a"}},
                        "all_": {"$firstN": {"n": 99, "input": "$a"}},
                    }
                }
            ]
        )
    )
    assert out == [{"f": [10, 20], "l": [40, 50], "all_": [10, 20, 30, 40, 50]}]


def test_aggregate_bitwise_operators(coll) -> None:
    from bson import Int64

    coll.insert_one({"_id": 1, "a": 12, "b": 10, "big": Int64(0xFF00)})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "and_": {"$bitAnd": ["$a", "$b"]},
                        "or_": {"$bitOr": ["$a", "$b", 1]},
                        "xor_": {"$bitXor": ["$a", "$b"]},
                        "not_": {"$bitNot": "$a"},
                        "long_": {"$bitAnd": ["$big", 255]},
                    }
                }
            ]
        )
    )
    assert out == [{"and_": 8, "or_": 15, "xor_": 6, "not_": -13, "long_": 0}]
    # An all-long operand keeps the long (int64) type over the wire.
    assert isinstance(out[0]["long_"], Int64)


def test_aggregate_group_topn_accumulators(coll) -> None:
    # Sort by score: x2(9) > x1(3) > x3(1). $topN/$top take the highest, $bottomN
    # the lowest end of the sort order; $top/$bottom are single values.
    coll.insert_many(
        [
            {"t": "a", "s": "x1", "score": 3},
            {"t": "a", "s": "x2", "score": 9},
            {"t": "a", "s": "x3", "score": 1},
        ]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$group": {
                        "_id": "$t",
                        "top2": {"$topN": {"n": 2, "sortBy": {"score": -1}, "output": "$s"}},
                        "bot2": {"$bottomN": {"n": 2, "sortBy": {"score": 1}, "output": "$s"}},
                        "hi": {"$top": {"sortBy": {"score": -1}, "output": "$s"}},
                        "lo": {"$bottom": {"sortBy": {"score": -1}, "output": "$s"}},
                    }
                }
            ]
        )
    )
    assert out == [{"_id": "a", "top2": ["x2", "x1"], "bot2": ["x1", "x2"], "hi": "x2", "lo": "x3"}]


def test_aggregate_group_nelem_accumulators(coll) -> None:
    # Group values in doc order: 3, 1, null, 5, 2. $firstN/$lastN keep the null;
    # $maxN/$minN drop it (matched to mongod).
    coll.insert_many(
        [
            {"g": "a", "v": 3},
            {"g": "a", "v": 1},
            {"g": "a", "v": None},
            {"g": "a", "v": 5},
            {"g": "a", "v": 2},
        ]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$group": {
                        "_id": "$g",
                        "first3": {"$firstN": {"n": 3, "input": "$v"}},
                        "last2": {"$lastN": {"n": 2, "input": "$v"}},
                        "max2": {"$maxN": {"n": 2, "input": "$v"}},
                        "min2": {"$minN": {"n": 2, "input": "$v"}},
                    }
                }
            ]
        )
    )
    assert out == [
        {"_id": "a", "first3": [3, 1, None], "last2": [5, 2], "max2": [5, 3], "min2": [1, 2]}
    ]


def test_aggregate_group_stddev(coll) -> None:
    # Values 2,4,6: population variance (4+0+4)/3 -> stdDevPop sqrt(8/3);
    # sample variance 8/2 = 4 -> stdDevSamp 2.0.
    coll.insert_many([{"g": "x", "v": 2}, {"g": "x", "v": 4}, {"g": "x", "v": 6}])
    out = list(
        coll.aggregate(
            [
                {
                    "$group": {
                        "_id": "$g",
                        "pop": {"$stdDevPop": "$v"},
                        "samp": {"$stdDevSamp": "$v"},
                    }
                }
            ]
        )
    )
    assert out[0]["pop"] == (8.0 / 3.0) ** 0.5
    assert out[0]["samp"] == 2.0


def test_aggregate_group_merge_objects(coll) -> None:
    # $mergeObjects as a $group accumulator merges each operand doc across the
    # group (later keys override earlier); null/missing operands are skipped; an
    # all-missing group still yields {}.
    coll.insert_many(
        [
            {"g": "x", "sub": {"a": 1, "b": 1}},
            {"g": "x", "sub": {"b": 2, "c": 3}},  # b overrides, c adds
            {"g": "x"},  # missing sub -> skipped
            {"g": "x", "sub": None},  # null sub -> skipped
            {"g": "y"},  # whole group missing/null -> {}
        ]
    )
    out = sorted(
        coll.aggregate([{"$group": {"_id": "$g", "m": {"$mergeObjects": "$sub"}}}]),
        key=lambda d: d["_id"],
    )
    assert out == [
        {"_id": "x", "m": {"a": 1, "b": 2, "c": 3}},
        {"_id": "y", "m": {}},
    ]


def test_aggregate_unwind_stage(coll) -> None:
    coll.insert_one({"_id": 1, "tags": ["a", "b", "c"]})
    out = list(coll.aggregate([{"$unwind": "$tags"}]))
    assert [d["tags"] for d in out] == ["a", "b", "c"]


def test_aggregate_unwind_validation_via_pymongo(coll) -> None:
    """$unwind: bare path 28818, non-string path 28808, non-string/empty index
    28810, $-prefixed index 28822, non-bool preserve 28809. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": [1, 2, 3]})
    for spec, code in [
        ({"path": "a"}, 28818),
        ({"path": 5}, 28808),
        ({"path": "$a", "includeArrayIndex": 5}, 28810),
        ({"path": "$a", "includeArrayIndex": "$i"}, 28822),
        ({"path": "$a", "preserveNullAndEmptyArrays": 5}, 28809),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$unwind": spec}]))
        assert exc.value.code == code, spec
    assert len(list(coll.aggregate([{"$unwind": "$a"}]))) == 3


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


def test_aggregate_group_accumulator_mixed_types(coll) -> None:
    """$sum/$avg ignore non-numeric values; $min/$max order by BSON cross-type
    (bool > string > number) and skip null/missing — matching mongod."""
    from bson.int64 import Int64

    coll.insert_many(
        [
            {"_id": 1, "v": 10},
            {"_id": 2, "v": "hi"},
            {"_id": 3, "v": True},
            {"_id": 4, "v": None},
            {"_id": 5},
            {"_id": 6, "v": 2.5},
            {"_id": 7, "v": Int64(3)},
        ]
    )
    [b] = list(
        coll.aggregate(
            [
                {
                    "$group": {
                        "_id": None,
                        "s": {"$sum": "$v"},
                        "a": {"$avg": "$v"},
                        "mn": {"$min": "$v"},
                        "mx": {"$max": "$v"},
                    }
                }
            ]
        )
    )
    assert b["s"] == 15.5
    assert b["a"] == 15.5 / 3
    assert b["mn"] == 2.5
    assert b["mx"] is True
    # All-non-numeric group: $sum -> 0, $avg -> null.
    coll.delete_many({})
    coll.insert_many([{"v": "x"}, {"v": "y"}])
    [b2] = list(
        coll.aggregate([{"$group": {"_id": None, "s": {"$sum": "$v"}, "a": {"$avg": "$v"}}}])
    )
    assert b2["s"] == 0
    assert b2["a"] is None


def test_aggregate_push_addtoset_skip_missing(coll) -> None:
    """$push / $addToSet skip a missing field value (mongod semantics), keep null."""
    coll.insert_many(
        [
            {"_id": 1, "g": "a", "s": "x"},
            {"_id": 2, "g": "a"},  # missing s -> skipped
            {"_id": 3, "g": "a", "s": None},  # explicit null -> kept
            {"_id": 4, "g": "a", "s": "x"},
        ]
    )
    out = list(coll.aggregate([{"$group": {"_id": "$g", "p": {"$push": "$s"}}}]))
    assert out == [{"_id": "a", "p": ["x", None, "x"]}]
    out2 = list(coll.aggregate([{"$group": {"_id": "$g", "v": {"$addToSet": "$s"}}}]))
    v = out2[0]["v"]
    assert sorted(x for x in v if x is not None) == ["x"] and None in v


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


def test_aggregate_to_int_overflow_via_pymongo(coll) -> None:
    from pymongo.errors import OperationFailure

    coll.insert_one({"v": 3_000_000_000.0})  # > int32 max
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {"n": {"$toInt": "$v"}}}]))
    assert exc.value.code == 241

    # $convert with onError catches the overflow instead of erroring.
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "n": {"$convert": {"input": "$v", "to": "int", "onError": -1}},
                    }
                }
            ]
        )
    )
    assert out == [{"n": -1}]


def test_aggregate_to_long_via_pymongo(coll) -> None:
    """$toLong truncates toward zero, parses strings, and yields a 64-bit long
    (values beyond int32 are fine); overflow beyond int64 raises 241. mongod
    7.0.12-verified."""
    from bson.int64 import Int64
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "d": 2.7, "s": "42", "big": 9_000_000_000.0})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "a": {"$toLong": "$d"},
                        "b": {"$toLong": "$s"},
                        "c": {"$toLong": "$big"},
                    }
                }
            ]
        )
    )
    assert out == [{"a": 2, "b": 42, "c": 9_000_000_000}]
    assert all(isinstance(out[0][k], Int64) for k in ("a", "b", "c"))
    # Overflow beyond int64 -> 241, and onError catches it.
    coll.update_one({"_id": 1}, {"$set": {"huge": "99999999999999999999"}})
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {"n": {"$toLong": "$huge"}}}]))
    assert exc.value.code == 241
    got = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "n": {"$convert": {"input": "$huge", "to": "long", "onError": -1}},
                    }
                }
            ]
        )
    )
    assert got == [{"n": -1}]


def test_conversion_error_codes_via_pymongo(coll) -> None:
    """mongod-specific expression error codes over the wire: unparseable numeric
    string -> 241, unknown $convert target -> 2, $sortArray non-array -> 2942504,
    $strLenCP/$strLenBytes non-string -> 34471/34473. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "s": "abc", "n": 5})
    for expr, code in [
        ({"$toInt": "$s"}, 241),
        ({"$toDouble": "$s"}, 241),
        ({"$convert": {"input": "$n", "to": "bogus"}}, 2),
        ({"$sortArray": {"input": "$n", "sortBy": 1}}, 2942504),
        ({"$strLenCP": "$n"}, 34471),
        ({"$strLenBytes": "$n"}, 34473),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"v": expr}}]))
        assert exc.value.code == code, expr


def test_array_set_typeguard_codes_via_pymongo(coll) -> None:
    """Array/set operators reject non-array/non-object arguments with mongod's
    exact codes over the wire (incl. the previously-silent $arrayElemAt/$in).
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": 5})
    for expr, code in [
        ({"$size": "$n"}, 17124),
        ({"$arrayElemAt": ["$n", 0]}, 28689),
        ({"$in": [1, "$n"]}, 40081),
        ({"$setUnion": ["$n"]}, 17043),
        ({"$mergeObjects": ["$n"]}, 40400),
        ({"$anyElementTrue": "$n"}, 17041),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"v": expr}}]))
        assert exc.value.code == code, expr


def test_string_typeguard_codes_via_pymongo(coll) -> None:
    """String/binary operators reject non-string arguments with mongod's exact
    codes over the wire (incl. the previously-silent $regexMatch/$regexFind/
    $regexFindAll). mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": 5})
    for expr, code in [
        ({"$regexMatch": {"input": "$n", "regex": "a"}}, 51104),
        ({"$regexFind": {"input": "$n", "regex": "a"}}, 51104),
        ({"$indexOfBytes": ["$n", "a"]}, 40091),
        ({"$binarySize": "$n"}, 51276),
        ({"$bsonSize": "$n"}, 31393),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"v": expr}}]))
        assert exc.value.code == code, expr


def test_strcasecmp_coercion_via_pymongo(coll) -> None:
    """$strcasecmp coerces its operands to string (null -> ""), rejecting only
    bool (16007), over the wire. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": 5})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "a": {"$strcasecmp": ["$n", "a"]},
                        "b": {"$strcasecmp": [5, 10]},
                    }
                }
            ]
        )
    )
    assert out == [{"a": -1, "b": 1}]
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {"v": {"$strcasecmp": [True, "a"]}}}]))
    assert exc.value.code == 16007


def test_expression_accumulators_via_pymongo(coll) -> None:
    """$sum/$avg/$max/$min work as expression operators (MongoDB 5.0+) over the
    wire, not just as group accumulators. mongod 7.0.12-verified."""
    coll.insert_one({"_id": 1, "arr": [1, 2, 3], "n": 5})
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "s": {"$sum": "$arr"},
                        "a": {"$avg": "$arr"},
                        "mx": {"$max": "$arr"},
                        "mn": {"$min": "$arr"},
                        "sn": {"$sum": "$n"},
                    }
                }
            ]
        )
    )
    assert out == [{"s": 6, "a": 2.0, "mx": 3, "mn": 1, "sn": 5}]


def test_date_misc_typeguard_codes_via_pymongo(coll) -> None:
    """Date/misc operators match mongod's error codes over the wire (incl. the
    previously-silent $dateToString non-date and $dateDiff missing endDate).
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": 5})
    for expr, code in [
        ({"$dateToString": {"date": "$n"}}, 16006),
        ({"$dateFromString": {"dateString": "$n"}}, 241),
        ({"$switch": {"branches": []}}, 40068),
        ({"$ifNull": ["$n"]}, 1257300),
        ({"$getField": {"field": "$n", "input": {}}}, 5654602),
        ({"$dateDiff": {"startDate": "$$NOW"}}, 5166304),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"v": expr}}]))
        assert exc.value.code == code, expr


def test_more_expression_error_codes_via_pymongo(coll) -> None:
    """More mongod-specific expression error codes over the wire: $zip (34461/
    34468), $arrayToObject (40386), $objectToArray (40390), $replaceOne per-arg
    (51746/51745/51744), $dateDiff unknown unit (9). mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": 5, "d": "2020-01-01"})
    for expr, code in [
        ({"$zip": {"inputs": "$n"}}, 34461),
        ({"$zip": {"inputs": ["$n"]}}, 34468),
        ({"$arrayToObject": "$n"}, 40386),
        ({"$objectToArray": "$n"}, 40390),
        ({"$replaceOne": {"input": "$n", "find": "a", "replacement": "b"}}, 51746),
        ({"$replaceOne": {"input": "x", "find": "$n", "replacement": "b"}}, 51745),
        ({"$replaceAll": {"input": "x", "find": "y", "replacement": "$n"}}, 51744),
        (
            {"$dateDiff": {"startDate": "$$NOW", "endDate": "$$NOW", "unit": "bogus"}},
            9,
        ),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"v": expr}}]))
        assert exc.value.code == code, expr


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


def test_create_index_same_name_different_key_conflicts(coll) -> None:
    """Re-creating an index name with a different key spec is rejected with
    IndexKeySpecsConflict (86), matching mongod. mongo-cxx-driver's
    `create_index tests/fails` and `index_view/fails for same name` pin this."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"a": 1})
    coll.create_index([("a", 1)], name="myIndex")
    with pytest.raises(OperationFailure) as exc:
        coll.create_index([("a", -1)], name="myIndex")
    assert exc.value.code == 86
    assert exc.value.details.get("codeName") == "IndexKeySpecsConflict"


def test_create_index_same_name_different_options_conflicts(coll) -> None:
    """Same name + same key but different options → IndexOptionsConflict (85)."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"b": 1})
    coll.create_index([("b", 1)], name="b_idx")
    with pytest.raises(OperationFailure) as exc:
        coll.create_index([("b", 1)], name="b_idx", unique=True)
    assert exc.value.code == 85
    assert exc.value.details.get("codeName") == "IndexOptionsConflict"


def test_create_index_identical_recreate_is_noop(coll) -> None:
    """Re-creating an identical index (same key, name, options) is a no-op:
    the createIndexes reply carries `note: "all indexes already exist"` and the
    index count is unchanged — drivers (mongocxx's `fails for same keys and
    options`) report it as "already exists" rather than a fresh create."""
    coll.insert_one({"a": 1})
    coll.create_index([("a", 1)], name="a_1")
    reply = coll.database.command(
        "createIndexes", coll.name, indexes=[{"key": {"a": 1}, "name": "a_1"}]
    )
    assert reply["note"] == "all indexes already exist"
    assert reply["numIndexesBefore"] == reply["numIndexesAfter"]


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


def test_create_unique_index_with_dropdups_ignores_option_and_fails_on_dup(coll) -> None:
    """``dropDups`` was removed in MongoDB 3.0; mongod accepts but ignores it
    rather than rejecting the spec as an unknown field. So a unique index over
    duplicate data still fails on the duplicate (DuplicateKeyError), the docs
    are untouched, and no index is created. Mirrors pymongo's
    test_collection.test_index_dont_drop_dups."""
    from pymongo.errors import DuplicateKeyError as PyDup

    coll.insert_many([{"i": 1}, {"i": 2}, {"i": 2}, {"i": 3}])  # duplicate i
    with pytest.raises(PyDup):
        coll.create_index([("i", pymongo.ASCENDING)], unique=True, dropDups=False)
    # The duplicate wasn't dropped, and the unique index was never created.
    assert coll.count_documents({}) == 4
    assert len(coll.index_information()) == 1  # only the default _id_ index


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


def test_create_indexes_rejects_invalid_wildcard_projection(client: MongoClient) -> None:
    # mongo-ruby-driver's `create_one ... invalid wildcardProjection`
    # / `wildcard projection to an invalid base index` specs match
    # the error messages by regex. Real mongod rejects with
    # CannotCreateIndex (67) when the option is malformed or applied
    # to a non-wildcard base index.
    from pymongo.errors import OperationFailure

    db = client["wcp_validation_db"]

    # Non-doc wildcardProjection (test sends `wildcard_projection: 5`).
    with pytest.raises(OperationFailure) as exc:
        db.command(
            {
                "createIndexes": "things",
                "indexes": [{"key": {"$**": 1}, "name": "wild_int", "wildcardProjection": 5}],
            }
        )
    assert "wildcardProjection" in str(exc.value)
    assert "non-empty object" in str(exc.value)

    # Empty doc wildcardProjection.
    with pytest.raises(OperationFailure) as exc:
        db.command(
            {
                "createIndexes": "things",
                "indexes": [{"key": {"$**": 1}, "name": "wild_empty", "wildcardProjection": {}}],
            }
        )
    assert "non-empty object" in str(exc.value)

    # wildcardProjection on a non-wildcard base index.
    with pytest.raises(OperationFailure) as exc:
        db.command(
            {
                "createIndexes": "things",
                "indexes": [
                    {
                        "key": {"x": 1},
                        "name": "x_with_wcp",
                        "wildcardProjection": {"rating": 1},
                    }
                ],
            }
        )
    assert "only allowed" in str(exc.value)

    # Valid wildcardProjection on a wildcard key — accepted.
    db.command(
        {
            "createIndexes": "things",
            "indexes": [
                {
                    "key": {"$**": 1},
                    "name": "wild_ok",
                    "wildcardProjection": {"rating": 1},
                }
            ],
        }
    )


def test_coll_stats_storage_stats_surfaces_capped_bounds(client: MongoClient) -> None:
    # mongo-ruby-driver's ``Collection#create ... when the collection
    # is capped ... applies the options`` spec runs
    # ``coll.aggregate([{$collStats: {storageStats: {}}}])`` and reads
    # ``storageStats.{capped, max, maxSize}``. Real mongod renames the
    # user-set ``size`` to ``maxSize`` so callers can distinguish the
    # current data size from the cap.
    db = client["cs_capped_db"]
    db.create_collection("things", capped=True, size=4096, max=512)
    cs = list(db["things"].aggregate([{"$collStats": {"storageStats": {}}}]))
    assert len(cs) == 1
    storage_stats = cs[0]["storageStats"]
    assert storage_stats["capped"] is True
    assert storage_stats["max"] == 512
    assert storage_stats["maxSize"] == 4096

    # Non-capped collection: ``capped`` field absent (real mongod
    # omits these fields entirely on uncapped colls).
    db.create_collection("plain")
    cs = list(db["plain"].aggregate([{"$collStats": {"storageStats": {}}}]))
    assert "capped" not in cs[0]["storageStats"]
    assert "max" not in cs[0]["storageStats"]
    assert "maxSize" not in cs[0]["storageStats"]


def test_replies_gossip_cluster_time(client: MongoClient) -> None:
    # Real mongod attaches ``$clusterTime`` + ``operationTime`` to every
    # reply on a replica set. pymongo's change-stream tests read
    # ``reply["operationTime"]`` for ``startAtOperationTime``; causal
    # consistency reads ``$clusterTime``. Both must be present on reads,
    # writes, and admin commands alike.
    from bson import Timestamp

    db = client["gossip_test"]
    for reply in (
        client.admin.command("ping"),
        db.command("insert", "c", documents=[{"_id": 1}]),
        db.command("find", "c"),
    ):
        ct = reply["$clusterTime"]
        assert isinstance(ct["clusterTime"], Timestamp)
        assert ct["signature"]["keyId"] == 0
        assert isinstance(reply["operationTime"], Timestamp)

    # A write advances the cluster clock; its reply's operationTime must
    # be at least the pre-write gossiped time (causal ordering).
    before = client.admin.command("ping")["operationTime"]
    after = db.command("insert", "c", documents=[{"_id": 2}])["operationTime"]
    assert after >= before


def test_no_cluster_time_gossip_on_standalone(tmp_path) -> None:
    # Standalone mongod does not gossip cluster time; neither do we when
    # the replica-set persona is switched off.
    standalone_srv = SecantusDBServer(
        port=0, storage_path=str(tmp_path / "nogossip"), replica_set_name=None
    )
    standalone_srv.start()
    try:
        mc = MongoClient(standalone_srv.uri, serverSelectionTimeoutMS=2000, directConnection=True)
        reply = mc.admin.command("ping")
        assert "$clusterTime" not in reply
        assert "operationTime" not in reply
        mc.close()
    finally:
        standalone_srv.stop()


def test_change_stream_rejected_on_standalone(tmp_path) -> None:
    # When SecantusDB is booted in standalone mode (no replica-set
    # advertisement in ``hello``), opening a change stream must
    # fail with code 40573 — mongo-java-driver's ``change-streams-
    # errors.yml`` single-topology test asserts exactly this. The
    # default replica-set mode still permits change streams.
    from pymongo.errors import OperationFailure

    standalone_srv = SecantusDBServer(
        port=0, storage_path=str(tmp_path / "stand"), replica_set_name=None
    )
    standalone_srv.start()
    try:
        mc = MongoClient(standalone_srv.uri, serverSelectionTimeoutMS=2000, directConnection=True)
        with pytest.raises(OperationFailure) as exc:
            mc["db"]["c"].watch()
        assert exc.value.code == 40573
        assert "replica sets" in str(exc.value)
        mc.close()
    finally:
        standalone_srv.stop()


def test_configure_failpoint_block_connection(client: MongoClient) -> None:
    # mongo-node-driver's ``explain with timeoutMS`` CSOT tests configure
    # a ``failCommand`` with ``blockConnection: true, blockTimeMS: 500``
    # so the client-side ``timeoutMS`` timer fires before the server
    # responds. Verify the failpoint actually blocks before dispatching
    # the matched command.
    import time

    db = client["block_db"]
    db.create_collection("things")

    # Install a 500 ms block on ``find``.
    client.admin.command(
        {
            "configureFailPoint": "failCommand",
            "mode": {"times": 1},
            "data": {"failCommands": ["find"], "blockConnection": True, "blockTimeMS": 500},
        }
    )

    # Find should block ~500 ms then return normally.
    start = time.monotonic()
    list(db["things"].find())
    elapsed_ms = (time.monotonic() - start) * 1000
    assert 400 <= elapsed_ms <= 1500, f"expected ~500ms block, got {elapsed_ms:.0f}ms"

    # Second find — failpoint exhausted (``times: 1``), should be fast.
    start = time.monotonic()
    list(db["things"].find())
    elapsed_ms = (time.monotonic() - start) * 1000
    assert elapsed_ms < 200, f"second find should be unblocked, took {elapsed_ms:.0f}ms"


def test_get_parameter_advertises_only_scram_auth_mechanism(client: MongoClient) -> None:
    # Real mongod exposes enabled SASL mechanisms via
    # ``getParameter authenticationMechanisms``. mongo-java-driver's
    # unified ``RunOnRequirementsMatcher`` reads this to decide whether
    # to skip a test gated on ``authMechanism: "MONGODB-OIDC"``. We
    # support only SCRAM-SHA-256, so the array must list just that —
    # listing OIDC would lie to the driver and unskip tests we can't
    # actually pass.
    reply = client.admin.command({"getParameter": 1, "authenticationMechanisms": 1})
    assert reply["ok"] == 1.0
    assert reply["authenticationMechanisms"] == ["SCRAM-SHA-256"]

    # ``getParameter: "*"`` should also include it.
    reply = client.admin.command({"getParameter": "*"})
    assert "SCRAM-SHA-256" in reply["authenticationMechanisms"]
    # We do NOT support OIDC / X509 / GSSAPI / PLAIN — those must
    # stay out of the list so driver gauges self-skip correctly.
    for unsupported in ("MONGODB-OIDC", "MONGODB-X509", "GSSAPI", "PLAIN"):
        assert unsupported not in reply["authenticationMechanisms"]


def test_snapshot_read_concern_accepted_on_replica_set_persona(client: MongoClient) -> None:
    # mongod 5.0+ replica sets accept ``snapshot`` on exactly
    # find / aggregate / distinct (snapshot sessions); the reply
    # carries ``atClusterTime`` so pymongo can pin the session.
    # Other commands keep mongod's rejection.
    from pymongo.errors import OperationFailure

    db = client["snapshot_rc_db"]
    db.create_collection("things")

    reply = db.command({"find": "things", "readConcern": {"level": "snapshot"}})
    assert reply["cursor"]["atClusterTime"] is not None
    reply = db.command(
        {"aggregate": "things", "pipeline": [], "cursor": {}, "readConcern": {"level": "snapshot"}}
    )
    assert reply["cursor"]["atClusterTime"] is not None
    reply = db.command({"distinct": "things", "key": "x", "readConcern": {"level": "snapshot"}})
    assert reply["atClusterTime"] is not None

    with pytest.raises(OperationFailure) as exc:
        db.command({"count": "things", "readConcern": {"level": "snapshot"}})
    assert exc.value.code == 246
    assert "snapshot" in str(exc.value).lower()


def test_snapshot_read_concern_rejected_on_standalone(tmp_path) -> None:
    # With the replica-set persona off, mongod-on-standalone semantics
    # apply: snapshot is rejected with 246 SnapshotUnavailable on every
    # command. The mongo-java-driver
    # ``snapshot-sessions-not-supported-server-error`` unified spec
    # asserts this error shape.
    from pymongo.errors import OperationFailure

    with SecantusDBServer(
        port=0, storage_path=str(tmp_path / "wt-standalone"), replica_set_name=None
    ) as server:
        mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000, directConnection=True)
        try:
            db = mc["snapshot_rc_db"]
            db.create_collection("things")
            for cmd in (
                {"find": "things", "readConcern": {"level": "snapshot"}},
                {
                    "aggregate": "things",
                    "pipeline": [],
                    "cursor": {},
                    "readConcern": {"level": "snapshot"},
                },
                {"distinct": "things", "key": "x", "readConcern": {"level": "snapshot"}},
            ):
                with pytest.raises(OperationFailure) as exc:
                    db.command(cmd)
                assert exc.value.code == 246
                assert "snapshot" in str(exc.value).lower()
            # Other levels still accepted.
            reply = db.command({"find": "things", "readConcern": {"level": "majority"}})
            assert reply["ok"] == 1.0
        finally:
            mc.close()


def test_aggregate_linearizable_rc_rejects_write_stage(client: MongoClient) -> None:
    # mongod rejects aggregate with `$out` or `$merge` under
    # `readConcern: linearizable` with InvalidOptions (72). The
    # mongo-rust-driver `aggregate-out-readConcern` unified spec
    # asserts the operation errors. SecantusDB pretends to be a
    # replica-set primary so the unified runner doesn't skip the
    # test on topology; we must mirror mongod's rejection.
    from pymongo.errors import OperationFailure

    db = client["agg_lin_rc_db"]
    db.create_collection("src")
    db["src"].insert_many([{"_id": 1, "x": 11}, {"_id": 2, "x": 22}])

    for stage in ({"$out": "dst"}, {"$merge": {"into": "dst"}}):
        with pytest.raises(OperationFailure) as exc:
            db.command(
                {
                    "aggregate": "src",
                    "pipeline": [stage],
                    "cursor": {},
                    "readConcern": {"level": "linearizable"},
                }
            )
        assert exc.value.code == 72
        assert "linearizable" in str(exc.value).lower()

    # Same pipelines run cleanly under non-linearizable readConcern.
    db.command(
        {
            "aggregate": "src",
            "pipeline": [{"$out": "dst"}],
            "cursor": {},
            "readConcern": {"level": "majority"},
        }
    )


def test_list_indexes_rejects_negative_batch_size(client: MongoClient) -> None:
    # Real mongod rejects negative batchSize with BadValue.
    # mongo-ruby-driver's `failed_operation using a session` shared
    # spec for `indexes` passes `batch_size: -100` specifically to
    # provoke this rejection.
    from pymongo.errors import OperationFailure

    db = client["lib_bs_db"]
    db.command({"create": "things"})
    with pytest.raises(OperationFailure) as exc:
        db.command({"listIndexes": "things", "cursor": {"batchSize": -100}})
    assert "batchSize" in str(exc.value)
    assert ">= 0" in str(exc.value)

    # batchSize=0 is valid (returns empty firstBatch + open cursor).
    reply = db.command({"listIndexes": "things", "cursor": {"batchSize": 0}})
    assert reply["ok"] == 1.0


def test_create_indexes_rejects_unsupported_commit_quorum(client: MongoClient) -> None:
    # mongo-ruby-driver's commit_quorum unsupported-value tests match
    # the error message via regex: ``No write concern mode named
    # '<value>' found in replica set configuration``. Real mongod
    # surfaces unknown commitQuorum strings as a write-concern-mode
    # lookup miss (code 79, UnknownReplWriteConcern).
    from pymongo.errors import OperationFailure

    db = client["commit_quorum_db"]
    with pytest.raises(OperationFailure) as exc:
        db.command(
            {
                "createIndexes": "things",
                "indexes": [{"key": {"x": 1}, "name": "x_1"}],
                "commitQuorum": "unsupported-value",
            }
        )
    assert exc.value.code == 79
    assert "No write concern mode named" in str(exc.value)
    assert "unsupported-value" in str(exc.value)


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


def test_projection_positional_operator(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "items": [{"k": "a", "n": 1}, {"k": "b", "n": 2}, {"k": "c", "n": 3}]},
            {"_id": 2, "items": [{"k": "b", "n": 5}, {"k": "b", "n": 6}]},
        ]
    )
    # The query predicate on the array selects the first matching element.
    out = sorted(coll.find({"items.k": "b"}, {"items.$": 1}), key=lambda d: d["_id"])
    assert out == [
        {"_id": 1, "items": [{"k": "b", "n": 2}]},
        {"_id": 2, "items": [{"k": "b", "n": 5}]},
    ]


def test_projection_positional_scalar_and_elemmatch(coll) -> None:
    coll.insert_one({"_id": 1, "nums": [1, 5, 10, 15]})
    assert coll.find_one({"nums": {"$gte": 10}}, {"nums.$": 1}) == {"_id": 1, "nums": [10]}
    coll.replace_one({"_id": 1}, {"_id": 1, "items": [{"n": 1}, {"n": 9}]})
    got = coll.find_one({"items": {"$elemMatch": {"n": {"$gt": 5}}}}, {"items.$": 1})
    assert got == {"_id": 1, "items": [{"n": 9}]}


def test_projection_positional_errors(coll) -> None:
    coll.insert_one({"_id": 1, "items": [{"k": "b"}], "nums": [1, 2]})
    # >1 positional (validated at parse time — errors even with zero matches).
    with pytest.raises(pymongo.errors.OperationFailure) as e1:
        list(coll.find({"items.k": "zzz", "nums": 999}, {"items.$": 1, "nums.$": 1}))
    assert e1.value.code == 31276
    # Positional array not referenced by the query.
    with pytest.raises(pymongo.errors.OperationFailure) as e2:
        list(coll.find({"_id": 1}, {"items.$": 1}))
    assert e2.value.code == 51246


def test_explain_find_returns_query_planner(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    explanation = coll.find({"n": {"$gte": 2}}).explain()
    assert "queryPlanner" in explanation
    assert explanation["queryPlanner"]["namespace"].endswith(".things")
    assert "winningPlan" in explanation["queryPlanner"]
    assert "serverInfo" in explanation


def test_explain_verbosity_controls_execution_stats(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    db = coll.database
    inner = {"find": coll.name, "filter": {"n": {"$gte": 2}}}

    # queryPlanner: no executionStats at all.
    qp = db.command({"explain": inner, "verbosity": "queryPlanner"})
    assert "queryPlanner" in qp
    assert "executionStats" not in qp

    # executionStats: executionStats present, but NO allPlansExecution.
    es = db.command({"explain": inner, "verbosity": "executionStats"})
    assert "executionStats" in es
    assert "allPlansExecution" not in es["executionStats"]

    # allPlansExecution: executionStats present WITH an allPlansExecution array.
    ape = db.command({"explain": inner, "verbosity": "allPlansExecution"})
    assert "allPlansExecution" in ape["executionStats"]
    assert isinstance(ape["executionStats"]["allPlansExecution"], list)


def test_explain_aggregate_allplansexecution_present(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(5)])
    inner = {"aggregate": coll.name, "pipeline": [{"$match": {"n": {"$gte": 2}}}], "cursor": {}}
    res = coll.database.command({"explain": inner, "verbosity": "allPlansExecution"})
    # Aggregate-explain surfaces executionStats both top-level and per-$cursor.
    assert "allPlansExecution" in res["executionStats"]
    cursor_stats = res["stages"][0]["$cursor"]["executionStats"]
    assert "allPlansExecution" in cursor_stats


def test_aggregate_inline_explain_returns_plan_not_data(coll) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(3)])
    # The legacy inline ``explain: true`` flag on the aggregate command must
    # return an explain document (stages / queryPlanner), not pipeline output.
    res = coll.database.command(
        {"aggregate": coll.name, "pipeline": [{"$match": {"_id": {"$ne": 1}}}], "explain": True}
    )
    assert "stages" in res or "queryPlanner" in res


def test_aggregate_inline_explain_does_not_run_out(coll, client) -> None:
    coll.insert_many([{"_id": i, "n": i} for i in range(3)])
    out_name = "explain_out_things"
    coll.database.command(
        {
            "aggregate": coll.name,
            "pipeline": [{"$match": {}}, {"$out": out_name}],
            "explain": True,
        }
    )
    # Explain must NOT execute the $out write stage.
    assert client["testdb"][out_name].count_documents({}) == 0


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


def test_explain_find_exists_true_uses_sparse_index(coll) -> None:
    coll.create_index("f", sparse=True)
    coll.insert_many(
        [
            {"_id": 1, "f": 10},
            {"_id": 2, "f": None},
            {"_id": 3},
            {"_id": 4, "f": [1, 2]},
            {"_id": 5},
        ]
    )
    # Correct results: present-but-null and arrays count as existing.
    got = sorted(d["_id"] for d in coll.find({"f": {"$exists": True}}))
    assert got == [1, 2, 4]
    # And the plan rides the sparse index at IXSCAN.
    plan = coll.find({"f": {"$exists": True}}).explain()["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "FETCH"
    assert plan["inputStage"]["stage"] == "IXSCAN"
    assert plan["inputStage"]["indexName"] == "f_1"


def test_explain_find_ixscan_with_compound_index(coll) -> None:
    coll.create_index([("a", 1), ("b", 1)])
    coll.insert_many([{"_id": i, "a": i, "b": i * 10} for i in range(5)])
    plan = coll.find({"a": 1, "b": 10}).explain()["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "FETCH"
    assert plan["inputStage"]["stage"] == "IXSCAN"
    assert plan["inputStage"]["keyPattern"] == {"a": 1, "b": 1}


def test_explain_find_id_equality_uses_id_index(coll) -> None:
    # Regression: `find({_id: x})` must be a primary-key point lookup
    # (IXSCAN on the implicit _id_ index), not a COLLSCAN.
    coll.insert_many([{"_id": i, "n": i} for i in range(20)])
    plan = coll.find({"_id": 7}).explain()["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "FETCH"
    inner = plan["inputStage"]
    assert inner["stage"] == "IXSCAN"
    assert inner["indexName"] == "_id_"
    assert inner["keyPattern"] == {"_id": 1}


def test_find_by_id_returns_correct_doc(coll) -> None:
    coll.insert_many([{"_id": i, "n": i * 2} for i in range(20)])
    assert coll.find_one({"_id": 5}) == {"_id": 5, "n": 10}
    assert coll.find_one({"_id": {"$eq": 5}}) == {"_id": 5, "n": 10}
    # $in by _id: results in ascending _id order, missing ids dropped.
    assert list(coll.find({"_id": {"$in": [5, 1, 99, 3]}})) == [
        {"_id": 1, "n": 2},
        {"_id": 3, "n": 6},
        {"_id": 5, "n": 10},
    ]
    assert coll.find_one({"_id": 12345}) is None


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


def test_coll_stats_reports_real_data_size(client: MongoClient) -> None:
    """collStats.size and avgObjSize reflect actual bson-encoded bytes."""
    db = client["coll_stats_size_db"]
    db["things"].insert_many([{"_id": i, "n": i} for i in range(5)])
    out = db.command("collStats", "things")
    assert out["count"] == 5
    assert out["size"] > 0
    assert out["storageSize"] == out["size"]
    assert out["avgObjSize"] > 0
    # Roughly avgObjSize == size / count.
    assert abs(out["avgObjSize"] - out["size"] / 5) < 1e-9


def test_coll_stats_reports_index_sizes(client: MongoClient) -> None:
    """Each created secondary index appears in indexSizes with a positive size,
    and _id_ is always included."""
    db = client["coll_stats_idx_db"]
    db["things"].create_index("n")
    db["things"].insert_many([{"_id": i, "n": i * 2} for i in range(4)])
    out = db.command("collStats", "things")
    assert "_id_" in out["indexSizes"]
    assert "n_1" in out["indexSizes"]
    assert out["indexSizes"]["_id_"] > 0
    assert out["indexSizes"]["n_1"] > 0
    assert out["totalIndexSize"] == sum(out["indexSizes"].values())
    # nindexes includes the implicit _id_ index, matching MongoDB.
    assert out["nindexes"] == 2


def test_db_stats_reports_real_data_and_index_size(client: MongoClient) -> None:
    db = client["db_stats_size_db"]
    db["a"].create_index("v")
    db["a"].insert_many([{"_id": i, "v": i} for i in range(3)])
    db["b"].insert_one({"_id": 1, "x": "hello"})
    out = db.command("dbStats")
    assert out["objects"] == 4
    assert out["dataSize"] > 0
    assert out["indexSize"] > 0
    assert out["totalSize"] == out["dataSize"] + out["indexSize"]
    assert out["avgObjSize"] > 0


def test_coll_stats_empty_collection(client: MongoClient) -> None:
    """avgObjSize is 0 for an empty collection (no division by zero)."""
    db = client["coll_stats_empty_db"]
    db.create_collection("empty")
    out = db.command("collStats", "empty")
    assert out["count"] == 0
    assert out["size"] == 0
    assert out["avgObjSize"] == 0


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


def test_aggregate_merge_when_matched_delete(client: MongoClient) -> None:
    db = client["merge_delete_db"]
    db["src"].insert_many([{"_id": 1}, {"_id": 2}])
    db["dst"].insert_many([{"_id": 1, "tag": "x"}, {"_id": 3, "tag": "untouched"}])
    # whenNotMatched=discard so the unmatched src doc (_id=2) doesn't
    # land in dst — the assertion is purely about the delete path.
    list(
        db["src"].aggregate(
            [
                {
                    "$merge": {
                        "into": "dst",
                        "whenMatched": "delete",
                        "whenNotMatched": "discard",
                    }
                }
            ]
        )
    )
    remaining = sorted(db["dst"].find(), key=lambda d: d["_id"])
    assert remaining == [{"_id": 3, "tag": "untouched"}]


def test_aggregate_merge_when_matched_pipeline(client: MongoClient) -> None:
    """Pipeline form runs per matched doc with $$new bound to the source."""
    db = client["merge_pipeline_db"]
    db["src"].insert_one({"_id": 1, "delta": 10})
    db["dst"].insert_one({"_id": 1, "total": 5})
    pipeline = [{"$addFields": {"total": {"$add": ["$total", "$$new.delta"]}}}]
    list(db["src"].aggregate([{"$merge": {"into": "dst", "whenMatched": pipeline}}]))
    [doc] = list(db["dst"].find())
    assert doc == {"_id": 1, "total": 15}


def test_aggregate_merge_pipeline_with_let_bindings(client: MongoClient) -> None:
    """$merge let exposes user vars to the pipeline form."""
    db = client["merge_let_db"]
    db["src"].insert_one({"_id": 1, "qty": 3})
    db["dst"].insert_one({"_id": 1, "price": 100})
    list(
        db["src"].aggregate(
            [
                {
                    "$merge": {
                        "into": "dst",
                        "let": {"multiplier": "$qty"},
                        "whenMatched": [
                            {"$addFields": {"total": {"$multiply": ["$price", "$$multiplier"]}}}
                        ],
                    }
                }
            ]
        )
    )
    [doc] = list(db["dst"].find())
    assert doc == {"_id": 1, "price": 100, "total": 300}


def test_aggregate_merge_when_not_matched_fail(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    db = client["merge_nm_fail_db"]
    db["src"].insert_one({"_id": 1, "n": 7})
    with pytest.raises(OperationFailure):
        list(db["src"].aggregate([{"$merge": {"into": "dst", "whenNotMatched": "fail"}}]))


def test_aggregate_merge_on_non_id_requires_unique_index(client: MongoClient) -> None:
    """Without a unique index on the on field, $merge must refuse."""
    from pymongo.errors import OperationFailure

    db = client["merge_nounique_db"]
    db["src"].insert_one({"_id": 1, "k": "alpha", "n": 1})
    db["dst"].insert_one({"_id": 9, "k": "alpha", "n": 99})
    with pytest.raises(OperationFailure):
        list(db["src"].aggregate([{"$merge": {"into": "dst", "on": "k"}}]))


def test_aggregate_merge_on_non_id_with_unique_index(client: MongoClient) -> None:
    db = client["merge_unique_db"]
    db["src"].insert_one({"_id": 1, "k": "alpha", "n": 1})
    db["dst"].insert_one({"_id": 9, "k": "alpha", "n": 99})
    db["dst"].create_index("k", unique=True)
    list(db["src"].aggregate([{"$merge": {"into": "dst", "on": "k"}}]))
    [doc] = list(db["dst"].find())
    # Matched on k="alpha" → existing doc gets shallow-merged with source.
    # _id of the matched doc is preserved.
    assert doc == {"_id": 9, "k": "alpha", "n": 1}


def test_aggregate_merge_cross_database(client: MongoClient) -> None:
    src_db = client["merge_xdb_src"]
    dst_db = client["merge_xdb_dst"]
    src_db["coll"].insert_one({"_id": 1, "v": 42})
    list(
        src_db["coll"].aggregate([{"$merge": {"into": {"db": "merge_xdb_dst", "coll": "target"}}}])
    )
    [doc] = list(dst_db["target"].find())
    assert doc == {"_id": 1, "v": 42}


def test_aggregate_merge_rejects_unknown_when_matched(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    db = client["merge_bad_wm_db"]
    db["src"].insert_one({"_id": 1})
    with pytest.raises(OperationFailure):
        list(db["src"].aggregate([{"$merge": {"into": "dst", "whenMatched": "nonsense"}}]))


def test_aggregate_fill_value_via_pymongo(client: MongoClient) -> None:
    db = client["fill_value_xd"]
    db["readings"].insert_many(
        [{"_id": 1, "v": 10}, {"_id": 2}, {"_id": 3, "v": None}, {"_id": 4, "v": 30}]
    )
    out = sorted(
        db["readings"].aggregate([{"$fill": {"output": {"v": {"value": 0}}}}]),
        key=lambda d: d["_id"],
    )
    assert [d["v"] for d in out] == [10, 0, 0, 30]


def test_aggregate_fill_locf_via_pymongo(client: MongoClient) -> None:
    db = client["fill_locf_xd"]
    db["readings"].insert_many(
        [
            {"_id": 1, "t": 1, "v": 10},
            {"_id": 2, "t": 2},
            {"_id": 3, "t": 3, "v": 30},
            {"_id": 4, "t": 4},
        ]
    )
    out = list(
        db["readings"].aggregate(
            [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "locf"}}}}]
        )
    )
    assert [(d["t"], d["v"]) for d in out] == [(1, 10), (2, 10), (3, 30), (4, 30)]


def test_aggregate_fill_linear_via_pymongo(client: MongoClient) -> None:
    db = client["fill_linear_xd"]
    db["readings"].insert_many(
        [
            {"_id": 1, "t": 0, "v": 0},
            {"_id": 2, "t": 1},
            {"_id": 3, "t": 2},
            {"_id": 4, "t": 4, "v": 40},
        ]
    )
    out = list(
        db["readings"].aggregate(
            [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "linear"}}}}]
        )
    )
    assert [(d["t"], d["v"]) for d in out] == [(0, 0), (1, 10), (2, 20), (4, 40)]


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


def test_unsatisfiable_write_concern_attaches_wce(client: MongoClient) -> None:
    # Real mongod with a 1-member replica set executes write commands but
    # attaches `writeConcernError` (code 100, CannotSatisfyWriteConcern)
    # when `w` is an int above member count. mongo-ruby-driver's
    # `Mongo::Collection#create ... applies the write concern` spec relies
    # on this: it raises `OperationFailure` because of the wce. pymongo's
    # `Database.command()` returns the raw doc rather than raising — check
    # the wce directly on the reply.
    db = client["wc_unsat_db"]
    # `create` with w:2 — within mongod's 0..50 parse range but unsatisfiable
    # on our single-node replica set: the collection IS created (the op runs)
    # and the reply carries writeConcernError. (w above 50 is a *parse* error
    # instead — see test_write_concern_w_above_50_is_parse_error.)
    reply = db.command({"create": "things", "writeConcern": {"w": 2}})
    assert reply["ok"] == 1.0
    wce = reply.get("writeConcernError")
    assert wce is not None, f"expected writeConcernError, got {reply!r}"
    assert wce["code"] == 100
    assert wce["codeName"] == "CannotSatisfyWriteConcern"
    assert "things" in db.list_collection_names()

    # `drop` with w:2 — same shape: op runs, wce attached.
    reply = db.command({"drop": "things", "writeConcern": {"w": 2}})
    assert reply["ok"] == 1.0
    assert reply.get("writeConcernError", {}).get("code") == 100
    assert "things" not in db.list_collection_names()

    # `w: 1` satisfiable — no wce.
    reply = db.command({"create": "ok_things", "writeConcern": {"w": 1}})
    assert reply["ok"] == 1.0
    assert "writeConcernError" not in reply
    assert "ok_things" in db.list_collection_names()

    # `w: "majority"` satisfiable on single-node (majority of 1 is 1).
    reply = db.command({"create": "majority_things", "writeConcern": {"w": "majority"}})
    assert reply["ok"] == 1.0
    assert "writeConcernError" not in reply
    assert "majority_things" in db.list_collection_names()


def test_write_concern_w_above_50_is_parse_error(client: MongoClient) -> None:
    """A numeric ``writeConcern.w`` above 50 (mongod's max voting-member count)
    is rejected at *parse* time with FailedToParse (9) — a top-level command
    error, not a ``writeConcernError`` attached to a success. This is the C
    driver's ``assert_wc_oob_error`` shape (server >= 4.3.3) that mongo-c-driver's
    /Collection/{drop,rename,index} + /Database/drop assert for ``w: 99``."""
    from pymongo.errors import OperationFailure

    db = client["wc_oob_db"]
    db.command({"create": "c"})

    def assert_oob(cmd: dict) -> None:
        with pytest.raises(OperationFailure) as exc:
            db.command(cmd)
        assert exc.value.code == 9, f"{cmd}: expected code 9, got {exc.value.code}"
        assert "not greater than 50" in str(exc.value)

    assert_oob(
        {
            "createIndexes": "c",
            "indexes": [{"key": {"a": 1}, "name": "a_1"}],
            "writeConcern": {"w": 99},
        }
    )
    assert_oob({"renameCollection": "wc_oob_db.c", "to": "wc_oob_db.c2", "writeConcern": {"w": 99}})
    assert_oob({"drop": "c2", "writeConcern": {"w": 99}})
    assert_oob({"dropDatabase": 1, "writeConcern": {"w": 99}})
    # Boundary: w == 50 is valid (parse-OK), so it's the unsatisfiable path, not
    # a parse error — a success with a writeConcernError, not an OperationFailure.
    db.command({"create": "fifty"})
    reply = db.command({"drop": "fifty", "writeConcern": {"w": 50}})
    assert reply["ok"] == 1.0
    assert reply.get("writeConcernError", {}).get("code") == 100


def test_unacknowledged_writes_do_not_desync_connection(server: SecantusDBServer) -> None:
    # `writeConcern: {w: 0}` triggers OP_MSG with the moreToCome flag set
    # — server must not reply. If it does, the next genuine response is
    # mis-paired and pymongo / Java driver close the connection with a
    # responseTo/requestId mismatch error.
    from pymongo.write_concern import WriteConcern

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll_w0 = mc["unack_db"]["things"].with_options(write_concern=WriteConcern(w=0))
        # Several fire-and-forget writes interleaved with normal commands
        # over the same connection. Pre-fix, the second normal command
        # would receive the moreToCome echo and bomb.
        for i in range(10):
            coll_w0.insert_one({"_id": i, "x": i * 2})
        # Acknowledged read on the same client must succeed.
        ack_coll = mc["unack_db"]["things"]
        docs = list(ack_coll.find().sort("_id"))
        assert [d["_id"] for d in docs] == list(range(10))
        # Ack write after w:0 writes — verifies connection still healthy.
        ack_coll.insert_one({"_id": 99})
        assert ack_coll.find_one({"_id": 99}) == {"_id": 99}
    finally:
        mc.close()


# ---- capped collections ----------------------------------------------------


def test_create_capped_surfaces_options_via_list_collections(client: MongoClient) -> None:
    db = client["capped_opts_db"]
    db.create_collection("logs", capped=True, size=4096, max=10)
    info = next(c for c in db.list_collections() if c["name"] == "logs")
    assert info["options"].get("capped") is True
    assert info["options"].get("size") == 4096
    assert info["options"].get("max") == 10


def test_list_collections_emits_info_uuid_and_idindex(client: MongoClient) -> None:
    """Each listCollections descriptor must carry ``info.uuid`` (BSON
    Binary subtype 4) and ``idIndex`` (mongod's implicit ``_id_`` index
    spec).

    Regression: mongo-go-driver's
    ``TestDatabase/list_collection_specifications/filter_passed_to_listCollections``
    reads both fields off the cursor and the ``ListCollectionSpecifications``
    helper requires them to populate its return value.
    """
    import bson as _bson

    db = client["lc_uuid_idx_db"]
    db.create_collection("widgets")
    db.create_collection("logs", capped=True, size=4096)
    specs = {c["name"]: c for c in db.list_collections()}
    for name in ("widgets", "logs"):
        spec = specs[name]
        info = spec["info"]
        assert info.get("readOnly") is False
        uuid_val = info.get("uuid")
        assert isinstance(uuid_val, _bson.Binary), (
            f"expected info.uuid to be bson.Binary, got {type(uuid_val).__name__}"
        )
        assert uuid_val.subtype == 4
        assert len(uuid_val) == 16
        id_index = spec["idIndex"]
        assert id_index == {
            "v": 2,
            "key": {"_id": 1},
            "name": "_id_",
            "ns": f"lc_uuid_idx_db.{name}",
        }


def test_list_collections_filter_on_options_capped(client: MongoClient) -> None:
    """``listCollections`` honours a server-side filter on dotted paths
    into nested descriptor fields (``options.capped`` here). The
    mongo-go-driver test of the same name relies on this — without the
    server-side filter, the driver would return every collection in the
    database and ``ListCollectionSpecifications`` would mis-report
    counts."""
    db = client["lc_filter_db"]
    db.create_collection("regular")
    db.create_collection("capped_one", capped=True, size=4096)
    db.create_collection("capped_two", capped=True, size=8192)
    matching = list(db.list_collections(filter={"options.capped": True}))
    names = sorted(c["name"] for c in matching)
    assert names == ["capped_one", "capped_two"]


def test_create_capped_without_size_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    db = client["capped_nosize_db"]
    with pytest.raises(OperationFailure) as exc:
        db.create_collection("logs", capped=True)
    assert exc.value.code == 72


def test_create_capped_negative_size_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    db = client["capped_negsize_db"]
    with pytest.raises(OperationFailure) as exc:
        db.create_collection("logs", capped=True, size=-1)
    assert exc.value.code == 72


def test_create_capped_zero_max_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    db = client["capped_zeromax_db"]
    with pytest.raises(OperationFailure) as exc:
        db.create_collection("logs", capped=True, size=4096, max=0)
    assert exc.value.code == 72


def test_create_uncapped_options_absent(client: MongoClient) -> None:
    db = client["uncapped_db"]
    db.create_collection("things")
    info = next(c for c in db.list_collections() if c["name"] == "things")
    assert "capped" not in info["options"]
    assert "size" not in info["options"]
    assert "max" not in info["options"]


def test_capped_collection_evicts_by_max_count(client: MongoClient) -> None:
    db = client["capped_evict_max_db"]
    db.create_collection("logs", capped=True, size=1_000_000, max=3)
    coll = db["logs"]
    for i in range(10):
        coll.insert_one({"i": i, "payload": "x"})
    docs = list(coll.find().sort("_id"))
    assert len(docs) == 3
    assert [d["i"] for d in docs] == [7, 8, 9]


def test_capped_collection_evicts_fifo_with_nonmonotonic_ids(client: MongoClient) -> None:
    # FIFO eviction must follow *insertion* order, not ``_id`` byte order.
    # With non-monotonic user ``_id`` values the two disagree: the doc-table
    # walk (``_id`` order) would evict the smallest ``_id`` first, whereas
    # mongod evicts the oldest-inserted first. Insert 5, 1, 9, 2, 7 (in that
    # order) into a max=3 capped collection; the survivors must be the last
    # three *inserted* (9, 2, 7 in insertion order), not the three highest
    # ``_id`` values (5, 7, 9).
    db = client["capped_fifo_nonmono_db"]
    db.create_collection("logs", capped=True, size=1_000_000, max=3)
    coll = db["logs"]
    insertion_order = [5, 1, 9, 2, 7]
    for _id in insertion_order:
        coll.insert_one({"_id": _id, "payload": "x"})
    survivors_natural = [d["_id"] for d in coll.find().hint({"$natural": 1})]
    assert survivors_natural == [9, 2, 7]
    assert {d["_id"] for d in coll.find()} == {9, 2, 7}


def test_capped_collection_evicts_by_size_budget(client: MongoClient) -> None:
    # Pick a size budget that fits about 3 docs of ~80 bytes each.
    db = client["capped_evict_size_db"]
    db.create_collection("logs", capped=True, size=300)
    coll = db["logs"]
    payload = "x" * 50
    for i in range(20):
        coll.insert_one({"i": i, "p": payload})
    docs = list(coll.find().sort("_id"))
    # The exact count depends on BSON encoding overhead, but it's bounded
    # and far smaller than 20.
    assert 0 < len(docs) <= 6
    # Survivors must be the most recent inserts.
    expected_tail = list(range(20 - len(docs), 20))
    assert [d["i"] for d in docs] == expected_tail


def test_capped_delete_is_allowed(client: MongoClient) -> None:
    db = client["capped_delete_db"]
    db.create_collection("logs", capped=True, size=1_000_000, max=10)
    coll = db["logs"]
    coll.insert_many([{"i": i} for i in range(5)])
    result = coll.delete_one({"i": 2})
    assert result.deleted_count == 1
    remaining = sorted(d["i"] for d in coll.find())
    assert remaining == [0, 1, 3, 4]


def test_capped_update_growth_triggers_eviction(client: MongoClient) -> None:
    # Tight size budget that fits the originals but not after growth.
    db = client["capped_update_grow_db"]
    db.create_collection("logs", capped=True, size=400)
    coll = db["logs"]
    for i in range(4):
        coll.insert_one({"i": i, "p": "x" * 30})
    before = list(coll.find().sort("_id"))
    assert len(before) == 4
    # Grow the youngest doc significantly: existing docs total ~size budget
    # already, so the larger doc should evict at least one of the older ones.
    coll.update_one({"i": 3}, {"$set": {"p": "y" * 200}})
    after = list(coll.find().sort("_id"))
    assert len(after) < len(before)
    # The grown doc must survive.
    grown = coll.find_one({"i": 3})
    assert grown is not None
    assert grown["p"] == "y" * 200


def test_capped_collection_eviction_emits_change_stream_deletes(
    client: MongoClient,
) -> None:
    db = client["capped_changestream_db"]
    db.create_collection("logs", capped=True, size=1_000_000, max=2)
    coll = db["logs"]
    coll.insert_one({"i": 0})
    coll.insert_one({"i": 1})
    # Open a stream then trigger a third insert that should evict i=0.
    with coll.watch() as stream:
        coll.insert_one({"i": 2})
        events = []
        # Expect insert + delete (eviction) within a small window.
        deadline = 5.0
        import time as _t

        start = _t.time()
        while _t.time() - start < deadline and len(events) < 2:
            event = stream.try_next()
            if event is not None:
                events.append(event)
        ops = [e["operationType"] for e in events]
        assert "insert" in ops
        assert "delete" in ops


# ---- tailable cursors on capped collections --------------------------------


def test_tailable_await_delivers_initial_docs_past_first_batch(client: MongoClient) -> None:
    """find().tailable_await() with batchSize < initial-match-count must deliver
    the rest via getMore, not throw them away.

    Regression for the bug surfaced by mongo-go-driver's
    ``TestCursor_RemainingBatchLength/first_batch_is_non_empty``:
    ``_find_tailable`` used to set the producer's watermark to the
    last doc in the collection, silently dropping ``initial_docs[batch_size:]``.
    First batch worked, but the second batch was always empty —
    awaitData blocking then masked the loss as a tailable poll, so the
    client either looped forever (Go's ``cursor.Next``) or gave up
    after firstBatch (pymongo's ``StopIteration``).
    """
    db = client["tail_init_db"]
    db.tailcap.drop()
    db.create_collection("tailcap", capped=True, size=64 * 1024)
    db.tailcap.insert_many([{"x": i} for i in range(1, 6)])

    cur = db.tailcap.find(
        cursor_type=pymongo.CursorType.TAILABLE_AWAIT,
        batch_size=2,
    ).max_await_time_ms(100)
    cur.batch_size(2)

    seen = []
    deadline = dt.datetime.now() + dt.timedelta(seconds=5)
    while len(seen) < 5 and dt.datetime.now() < deadline:
        try:
            doc = cur.next()
        except StopIteration:
            break
        seen.append(doc["x"])
        if len(seen) == 5:
            break

    assert seen == [1, 2, 3, 4, 5], (
        f"expected all 5 seeded docs via firstBatch + getMore, got {seen}"
    )
    with contextlib.suppress(Exception):
        cur.close()


def test_tailable_await_picks_up_inserts_after_find(client: MongoClient) -> None:
    """After firstBatch drains, the producer must surface docs inserted
    *after* the find — the canonical tailable use case."""
    db = client["tail_follow_db"]
    db.tailcap.drop()
    db.create_collection("tailcap", capped=True, size=64 * 1024)
    db.tailcap.insert_one({"x": 1})

    cur = db.tailcap.find(
        cursor_type=pymongo.CursorType.TAILABLE_AWAIT,
        batch_size=10,
    ).max_await_time_ms(100)

    first = cur.next()
    assert first["x"] == 1

    # New insert after the find: the next getMore must surface it.
    db.tailcap.insert_one({"x": 2})

    second = None
    deadline = dt.datetime.now() + dt.timedelta(seconds=5)
    while second is None and dt.datetime.now() < deadline:
        try:
            second = cur.next()
        except StopIteration:
            break
    assert second is not None and second["x"] == 2, (
        f"tailable cursor did not surface follow-up insert, got {second!r}"
    )
    with contextlib.suppress(Exception):
        cur.close()


def test_tailable_capped_follows_inserts_with_nonmonotonic_ids(client: MongoClient) -> None:
    """A tailable cursor follows INSERTION order, not `_id` order.

    The follow-up `_id`s (20, 10) are all *smaller* than everything in the
    initial batch (500, 400, 300), so a watermark that tracked the encoded `_id`
    would filter them out and the cursor would stall forever. mongod delivers
    them — its tailable cursors follow the RecordId (insertion) order that capped
    FIFO eviction also uses. This is the conformance proof for the RecordId
    tailable anchor; the pre-change producer returns nothing here.
    """
    db = client["tail_nonmono_db"]
    db.tailcap.drop()
    db.create_collection("tailcap", capped=True, size=64 * 1024)
    db.tailcap.insert_many([{"_id": 500}, {"_id": 400}, {"_id": 300}])

    cur = db.tailcap.find(
        cursor_type=pymongo.CursorType.TAILABLE_AWAIT,
        batch_size=10,
    ).max_await_time_ms(100)

    seen = [cur.next()["_id"] for _ in range(3)]
    assert seen == [500, 400, 300], f"initial batch not in insertion order: {seen}"

    db.tailcap.insert_many([{"_id": 20}, {"_id": 10}])

    deadline = dt.datetime.now() + dt.timedelta(seconds=5)
    while len(seen) < 5 and dt.datetime.now() < deadline:
        try:
            seen.append(cur.next()["_id"])
        except StopIteration:
            break
    assert seen == [500, 400, 300, 20, 10], (
        f"tailable cursor dropped the smaller-_id follow-up inserts, got {seen}"
    )
    with contextlib.suppress(Exception):
        cur.close()


def test_tailable_await_filter_applies_to_follow_up_inserts(client: MongoClient) -> None:
    """The tailable producer must re-apply the find filter to docs inserted
    after the find — not just to firstBatch. Regression for the libmongoc
    ``/Collection/tailable/timeout/single`` failure: a TAILABLE_AWAIT cursor
    with a filter matching nothing was surfacing every later (and existing)
    doc, because the producer returned scanned rows unfiltered."""
    db = client["tail_filter_db"]
    db.tailcap.drop()
    db.create_collection("tailcap", capped=True, size=64 * 1024)
    db.tailcap.insert_one({"x": 1})  # does NOT match {a: 1}

    cur = db.tailcap.find(
        {"a": 1},
        cursor_type=pymongo.CursorType.TAILABLE_AWAIT,
        batch_size=10,
    ).max_await_time_ms(100)

    # No matching doc exists yet — the cursor must yield nothing and stay alive.
    with pytest.raises(StopIteration):
        cur.next()

    # A non-matching insert must remain invisible.
    db.tailcap.insert_one({"x": 2})
    with pytest.raises(StopIteration):
        cur.next()

    # A matching insert must surface.
    db.tailcap.insert_one({"a": 1, "tag": "hit"})
    found = None
    deadline = dt.datetime.now() + dt.timedelta(seconds=5)
    while found is None and dt.datetime.now() < deadline:
        try:
            found = cur.next()
        except StopIteration:
            continue
    assert found is not None and found.get("tag") == "hit", (
        f"tailable filter did not surface the matching insert, got {found!r}"
    )
    with contextlib.suppress(Exception):
        cur.close()


def test_tailable_capped_rollover_kills_cursor(client: MongoClient) -> None:
    """When a capped collection rolls over and evicts the document a tailable
    cursor is anchored on, mongod kills the cursor with CappedPositionLost
    (code 136). pymongo swallows that for tailable cursors, so a subsequent
    read returns no docs and ``cursor.alive`` is False — it must NOT keep
    streaming the post-rollover docs. Mirrors pymongo's test_cursor.test_tailable."""
    db = client["tail_rollover_db"]
    db.cap.drop()
    db.create_collection("cap", capped=True, size=4096, max=3)

    cursor = db.cap.find(cursor_type=pymongo.CursorType.TAILABLE)
    # Walk the cursor forward one doc at a time, anchoring it on x:3.
    for x in (1, 2, 3):
        db.cap.insert_one({"x": x})
        got = [d["x"] for d in cursor]
        assert got == [x], f"expected [{x}] this round, got {got}"

    # Rollover: max=3, so inserting 4,5,6 evicts 1,2,3 — including x:3, the
    # doc the cursor was anchored on. The cursor's position is lost.
    db.cap.insert_many([{"x": i} for i in range(4, 7)])
    assert cursor.to_list() == []
    assert cursor.alive is False
    assert db.cap.count_documents({}) == 3


def test_tailable_drop_returns_collection_dropped(client: MongoClient) -> None:
    """Dropping a capped collection out from under a tailable cursor makes the
    next getMore fail with QueryPlanKilled (175) and a "collection dropped"
    message — what mongod surfaces to a tailing client. mongo-php-driver's
    cursor-tailable_error-001 asserts the message mentions "collection
    dropped"; a bare CursorNotFound (which non-tailable cursors still get)
    would not satisfy it. Tested at the command level because pymongo's
    high-level cursor swallows 175 as a clean tailable close (see the
    companion test below)."""
    db = client["tail_drop_db"]
    db.cap.drop()
    db.create_collection("cap", capped=True, size=1024 * 1024)
    db.cap.insert_many([{"_id": i} for i in (1, 2, 3)])

    res = db.command("find", "cap", filter={}, tailable=True, batchSize=2)
    cid = res["cursor"]["id"]
    assert cid != 0

    db.command("drop", "cap")

    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command("getMore", cid, collection="cap")
    assert exc.value.code == 175, f"want QueryPlanKilled(175), got {exc.value.code}"
    assert "collection dropped" in str(exc.value)

    # A NON-tailable cursor whose collection is dropped still gets the plain
    # CursorNotFound (43) — mongo-c-driver's error_document/getmore depends
    # on it, so the tailable special-case must not bleed into regular cursors.
    db.reg.drop()
    db.reg.insert_many([{"_id": i} for i in range(5)])
    res2 = db.command("find", "reg", filter={}, batchSize=2)
    cid2 = res2["cursor"]["id"]
    db.command("drop", "reg")
    with pytest.raises(pymongo.errors.OperationFailure) as exc2:
        db.command("getMore", cid2, collection="reg")
    assert exc2.value.code == 43, f"want CursorNotFound(43), got {exc2.value.code}"


def test_tailable_drop_closes_pymongo_cursor_cleanly(client: MongoClient) -> None:
    """pymongo lists 175 (QueryPlanKilled) in ``_CURSOR_CLOSED_ERRORS``, so a
    high-level TAILABLE cursor whose collection is dropped simply stops
    iterating (``alive`` False) rather than raising — the server still emits
    the 175 the wire test above asserts; this pins the driver-visible
    behaviour."""
    db = client["tail_drop_clean_db"]
    db.cap.drop()
    db.create_collection("cap", capped=True, size=1024 * 1024)
    db.cap.insert_many([{"_id": i} for i in (1, 2, 3)])

    cursor = db.cap.find(cursor_type=pymongo.CursorType.TAILABLE).max_await_time_ms(50)
    drained = [cursor.next()["_id"] for _ in range(3)]
    assert drained == [1, 2, 3]

    db.command("drop", "cap")
    assert cursor.to_list() == []
    assert cursor.alive is False


def test_atlas_search_index_commands_rejected(client: MongoClient) -> None:
    """Atlas Search index management is Atlas-only. A non-Atlas mongod rejects
    the createSearchIndexes / updateSearchIndex / dropSearchIndex commands and
    the $listSearchIndexes aggregation stage with an error naming Atlas — the
    mongo-c-driver /index-management/{list,drop,update}SearchIndex tests assert
    the error mentions "Atlas". Previously these surfaced as CommandNotFound /
    unrecognized-stage, neither containing "Atlas"."""
    db = client["atlas_search_db"]
    db.coll.insert_one({"x": 1})

    # $listSearchIndexes aggregation stage.
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(db.coll.aggregate([{"$listSearchIndexes": {}}]))
    assert "Atlas" in str(exc.value)

    # The three search-index management commands.
    for cmd in (
        {"createSearchIndexes": "coll", "indexes": [{"name": "i", "definition": {}}]},
        {"updateSearchIndex": "coll", "name": "i", "definition": {}},
        {"dropSearchIndex": "coll", "name": "i"},
    ):
        with pytest.raises(pymongo.errors.OperationFailure) as exc:
            db.command(cmd)
        assert "Atlas" in str(exc.value), f"{next(iter(cmd))} error missing 'Atlas'"

    # A normal aggregation is unaffected.
    assert list(db.coll.aggregate([{"$match": {"x": 1}}, {"$project": {"_id": 0}}])) == [{"x": 1}]


# --- local.oplog.rs wire-surface (pymongo-driven) -------------------------


def test_oplog_rs_listed_via_pymongo(client: MongoClient) -> None:
    local = client["local"]
    names = set(local.list_collection_names())
    assert "oplog.rs" in names


def test_oplog_rs_capped_options_via_pymongo(client: MongoClient) -> None:
    local = client["local"]
    [info] = list(local.list_collections(filter={"name": "oplog.rs"}))
    opts = info.get("options") or {}
    assert opts.get("capped") is True
    assert isinstance(opts.get("size"), int) and opts["size"] > 0
    assert isinstance(opts.get("max"), int) and opts["max"] > 0


def test_oplog_rs_find_via_pymongo_sees_inserts(client: MongoClient) -> None:
    db = client["oplog_rs_xd"]
    db["things"].insert_many([{"_id": 1, "v": "alpha"}, {"_id": 2, "v": "beta"}])
    oplog = client["local"]["oplog.rs"]
    i_rows = list(oplog.find({"op": "i", "ns": "oplog_rs_xd.things"}))
    assert len(i_rows) >= 2
    ids = sorted(r["o"]["_id"] for r in i_rows)
    assert ids == [1, 2]


def test_oplog_rs_count_via_pymongo(client: MongoClient) -> None:
    db = client["oplog_rs_count_xd"]
    db["c"].insert_many([{"_id": i} for i in range(4)])
    oplog = client["local"]["oplog.rs"]
    n = oplog.count_documents({"op": "i", "ns": "oplog_rs_count_xd.c"})
    assert n == 4


def test_oplog_rs_sort_descending_via_pymongo(client: MongoClient) -> None:
    db = client["oplog_rs_sort_xd"]
    db["c"].insert_one({"_id": 1})
    db["c"].insert_one({"_id": 2})
    oplog = client["local"]["oplog.rs"]
    rows = list(oplog.find({"op": "i", "ns": "oplog_rs_sort_xd.c"}).sort("ts", -1).limit(2))
    assert rows[0]["o"]["_id"] == 2
    assert rows[1]["o"]["_id"] == 1


def test_oplog_rs_bootstrap_seed_never_empty(client: MongoClient) -> None:
    """mongod's oplog is never empty — its first entry is the replica set's
    "initiating set" noop. A fresh server seeds one so a client can tail
    ``local.oplog.rs`` before any user write."""
    oplog = client["local"]["oplog.rs"]
    rows = list(oplog.find().sort("$natural", pymongo.ASCENDING).limit(1))
    assert len(rows) == 1
    assert rows[0]["op"] == "n"  # noop bootstrap entry
    assert "ts" in rows[0]


def test_oplog_rs_tailable_await_reads_entries(client: MongoClient) -> None:
    """A TAILABLE_AWAIT cursor over ``local.oplog.rs`` reads entries the way
    replication does. Mirrors pymongo's test_cursor.test_to_list_tailable:
    take the latest ts via $natural DESC, then tail from it."""
    db = client["oplog_tail_xd"]
    db["c"].insert_one({"_id": 1})  # ensure at least one real op
    oplog = client["local"]["oplog.rs"]
    last = oplog.find().sort("$natural", pymongo.DESCENDING).limit(-1).next()
    ts = last["ts"]
    cur = oplog.find(
        {"ts": {"$gte": ts}},
        cursor_type=pymongo.CursorType.TAILABLE_AWAIT,
    ).max_await_time_ms(50)
    try:
        docs = []
        deadline = dt.datetime.now() + dt.timedelta(seconds=5)
        while not docs and dt.datetime.now() < deadline:
            docs = cur.to_list()
        assert len(docs) >= 1
        assert all("ts" in d for d in docs)
    finally:
        with contextlib.suppress(Exception):
            cur.close()


def test_oplog_rs_insert_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure) as exc_info:
        client["local"]["oplog.rs"].insert_one({"forged": True})
    assert exc_info.value.code == 13


def test_oplog_rs_update_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure):
        client["local"]["oplog.rs"].update_one({"op": "i"}, {"$set": {"x": 1}})


def test_oplog_rs_delete_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure):
        client["local"]["oplog.rs"].delete_many({})


def test_oplog_rs_drop_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure):
        client["local"].drop_collection("oplog.rs")


def test_oplog_rs_create_index_rejected(client: MongoClient) -> None:
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure):
        client["local"]["oplog.rs"].create_index("ns")


# --- killOp -----------------------------------------------------------------


def test_killop_unknown_opid_returns_ok(client: MongoClient) -> None:
    # mongod's killOp is fire-and-forget — returns ok=1 even when the
    # opid is unknown. We mirror that, surfacing what happened via
    # the ``info`` field so admin tooling can confirm.
    res = client.admin.command("killOp", op=999_999)
    assert res["ok"] == 1.0
    assert "no operation" in res["info"]


def test_killop_known_opid_closes_connection(server) -> None:
    """``killOp`` against a real opid shuts the connection's socket.

    Verifies via ``currentOp``: the victim's conn_id disappears from
    the in-progress list after the kill. (pymongo's pool silently
    reopens a new connection on the next command, so we can't rely on
    the client-side surfacing an error — the server-side registry is
    the ground truth.)
    """
    import time

    from pymongo import MongoClient

    victim = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        victim.admin.command("hello")  # force the handshake to land
        admin = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            inprog = admin.admin.command("currentOp").get("inprog", []) or []
            admin_addr = admin.address
            victim_id: int | None = None
            for r in inprog:
                if r.get("type") != "op":
                    continue
                if r.get("client") == f"{admin_addr[0]}:{admin_addr[1]}":
                    continue
                victim_id = int(r["opid"])
                break
            assert victim_id is not None, f"could not locate victim conn in {inprog!r}"

            res = admin.admin.command("killOp", op=victim_id)
            assert res["ok"] == 1.0
            assert "killed" in res["info"]

            # Poll currentOp until the killed conn is gone — server-side
            # registry is the ground truth (pymongo's pool transparently
            # reconnects without surfacing an error on the client side).
            deadline = time.monotonic() + 2.0
            inprog = admin.admin.command("currentOp").get("inprog", []) or []
            opids = {int(r["opid"]) for r in inprog if r.get("type") == "op"}
            while victim_id in opids and time.monotonic() < deadline:
                time.sleep(0.05)
                inprog = admin.admin.command("currentOp").get("inprog", []) or []
                opids = {int(r["opid"]) for r in inprog if r.get("type") == "op"}
            assert victim_id not in opids, (
                f"victim {victim_id} still in currentOp after killOp: {opids}"
            )
        finally:
            admin.close()
    finally:
        victim.close()


# --- secantusAdmin.backupArchive --------------------------------------------


def test_backup_archive_via_pymongo_round_trips_data(server, tmp_path) -> None:
    """End-to-end through the wire: insert → backupArchive → stop server →
    extract → boot new server pointing at the extracted dir → verify
    every doc + index + oplog entry is still there.
    """
    import tarfile

    from pymongo import MongoClient

    from secantus import SecantusDBServer

    archive = tmp_path / "round_trip.tar.gz"

    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        db = client["round_trip_xd"]
        db["things"].insert_many([{"_id": i, "v": f"row-{i}"} for i in range(20)])
        db["things"].create_index([("v", 1)], name="v_1", unique=True)
        db["things"].update_one({"_id": 5}, {"$set": {"v": "row-5-updated"}})

        res = client.admin.command("secantusAdmin.backupArchive", outputPath=str(archive))
        assert res["ok"] == 1.0
        assert res["path"] == str(archive)
        assert int(res["sizeBytes"]) > 0
        assert archive.exists()
    finally:
        client.close()

    restored_dir = tmp_path / "restored"
    restored_dir.mkdir()
    with tarfile.open(archive, "r:gz") as tar:
        # Same capability probe as ``Storage.extract_backup_archive`` — see the
        # fuller note there. ``filter="data"`` becomes Python 3.14's default, so
        # passing it explicitly pins today's behaviour to tomorrow's; the
        # ``hasattr`` guard keeps 3.10.11 (pre-backport) working.
        if hasattr(tarfile, "data_filter"):
            tar.extractall(restored_dir, filter="data")
        else:
            tar.extractall(restored_dir)

    with SecantusDBServer(port=0, storage_path=str(restored_dir)) as restored:
        client2 = MongoClient(restored.uri, serverSelectionTimeoutMS=2000)
        try:
            db2 = client2["round_trip_xd"]
            rows = sorted(db2["things"].find(), key=lambda d: d["_id"])
            assert len(rows) == 20
            assert rows[5]["v"] == "row-5-updated"
            assert all(rows[i]["v"] == f"row-{i}" for i in range(20) if i != 5)

            indexes = list(db2["things"].list_indexes())
            assert any(i["name"] == "v_1" for i in indexes)

            oplog_updates = list(
                client2["local"]["oplog.rs"].find({"op": "u", "ns": "round_trip_xd.things"})
            )
            assert len(oplog_updates) >= 1
        finally:
            client2.close()


def test_backup_archive_rejects_missing_output_path(client) -> None:
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure) as exc_info:
        client.admin.command("secantusAdmin.backupArchive")
    assert exc_info.value.code == 14


def test_oversized_documents_rejected_server_side(tmp_path) -> None:
    """mongod enforces maxBsonObjectSize per document server-side:
    insert -> 10334 BSONObjectTooLarge, upsert -> 17420, update that
    grows a doc over the cap -> 10334. Just-under documents succeed.
    Wording oracle-pinned against real mongod."""
    import pytest
    from pymongo import MongoClient
    from pymongo.errors import BulkWriteError, OperationFailure

    from secantus import SecantusDBServer

    max_size = 16 * 1024 * 1024
    big = "x" * max_size
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            coll = client["shop"]["big"]

            with pytest.raises(OperationFailure) as exc:
                coll.insert_one({"foo": big})
            assert exc.value.code == 10334
            assert "object to insert too large" in str(exc.value)

            with pytest.raises(BulkWriteError) as bexc:
                coll.insert_many([{"_id": 1, "x": 1}, {"foo": big}])
            assert bexc.value.details["nInserted"] == 1
            assert bexc.value.details["writeErrors"][0]["code"] == 10334

            with pytest.raises(OperationFailure) as uexc:
                coll.replace_one({"missing": 1}, {"foo": big}, upsert=True)
            assert uexc.value.code == 17420

            coll.insert_one({"_id": 2, "bar": "x"})
            with pytest.raises(OperationFailure) as gexc:
                coll.replace_one({"_id": 2}, {"bar": "x" * (max_size - 14)})
            assert gexc.value.code == 10334
            assert "Resulting document after update" in str(gexc.value)

            # Just under the cap succeeds end-to-end.
            coll.replace_one({"_id": 2}, {"bar": "x" * (max_size - 64)})
            fetched = coll.find_one({"_id": 2})
            assert fetched is not None and len(fetched["bar"]) == max_size - 64
        finally:
            client.close()


def test_upsert_with_none_id_reports_did_upsert(tmp_path) -> None:
    """An upsert whose resulting _id is None must still report the
    upsert — None is a valid _id, not a 'no upsert' sentinel.
    Oracle-pinned: pymongo's test_update_result asserts did_upsert."""
    from pymongo import MongoClient

    from secantus import SecantusDBServer

    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            coll = client["db"]["test"]
            r = coll.update_one({"_id": None, "x": 0}, {"$inc": {"x": 1}}, upsert=True)
            assert r.did_upsert is True
            assert r.upserted_id is None
            assert coll.find_one({"_id": None}) == {"_id": None, "x": 1}

            # A plain non-upserting update reports did_upsert False.
            r2 = coll.update_one({"_id": None}, {"$inc": {"x": 1}})
            assert r2.did_upsert is False

            # findOneAndUpdate upsert with _id None + return new.
            from pymongo import ReturnDocument

            doc = client["db"]["fam"].find_one_and_update(
                {"_id": None},
                {"$set": {"v": 1}},
                upsert=True,
                return_document=ReturnDocument.AFTER,
            )
            assert doc == {"_id": None, "v": 1}
        finally:
            client.close()


def test_cursor_min_max_index_bounds(tmp_path) -> None:
    """Cursor min()/max() bound a hinted index scan: max is an exclusive
    upper bound, min an inclusive lower bound. Oracle-pinned against a
    real mongod 2026-06-13."""
    from pymongo import MongoClient
    from pymongo.errors import OperationFailure

    from secantus import SecantusDBServer

    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            t = client["db"]["t"]
            t.create_index([("j", 1)])
            t.insert_many([{"j": j, "k": j} for j in range(10)])

            # max exclusive: j < 3 -> 3 docs.
            assert len(t.find().max([("j", 3)]).hint([("j", 1)]).to_list()) == 3
            # min inclusive: j >= 3 -> 7 docs.
            assert len(t.find().min([("j", 3)]).hint([("j", 1)]).to_list()) == 7
            # min + max: 2 <= j < 5 -> j in {2,3,4}.
            got = t.find().min([("j", 2)]).max([("j", 5)]).hint([("j", 1)]).to_list()
            assert sorted(d["j"] for d in got) == [2, 3, 4]

            # Compound index, full key.
            t.create_index([("j", 1), ("k", 1)])
            assert len(t.find().max([("j", 3), ("k", 3)]).hint([("j", 1), ("k", 1)]).to_list()) == 3

            # Wrong field order vs the hinted index -> OperationFailure.
            with pytest.raises(OperationFailure):
                t.find().max([("k", 3), ("j", 3)]).hint([("j", 1), ("k", 1)]).to_list()

            # Hint that doesn't correspond to an index -> OperationFailure.
            with pytest.raises(OperationFailure):
                t.find().max([("k", 3)]).hint("nonexistent").to_list()
        finally:
            client.close()


def test_upsert_with_subdocument_id(tmp_path) -> None:
    """An upsert whose filter pins ``_id`` to a SUBDOCUMENT value must
    seed that _id, not generate a fresh ObjectId. The seed extraction
    must distinguish a literal subdocument value ({f, f2}) from an
    operator expression ({$gt: 5}). Oracle-pinned (pymongo's
    test_upsert_uuid_standard_subdocuments)."""
    from pymongo import MongoClient

    from secantus import SecantusDBServer

    sub_id = {"f": b"x", "f2": 7}
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            coll = client["db"]["t"]
            r = coll.update_one({"_id": sub_id}, {"$set": {"a": 0}}, upsert=True)
            assert r.did_upsert is True
            assert r.upserted_id == sub_id
            assert coll.find_one({"_id": sub_id}) == {"_id": sub_id, "a": 0}

            # Operator-expression filter fields are still NOT seeded.
            r2 = coll.update_one({"n": {"$gt": 5}}, {"$set": {"b": 1}}, upsert=True)
            doc = coll.find_one({"_id": r2.upserted_id})
            assert "n" not in doc and doc["b"] == 1
        finally:
            client.close()


def test_drop_collection_honors_unsatisfiable_write_concern(tmp_path) -> None:
    """``drop`` with an unsatisfiable write concern (w > member count)
    raises a WriteConcernError, like other write commands on the
    single-node replica-set persona. Oracle: pymongo's
    test_drop_collection with IMPOSSIBLE_WRITE_CONCERN."""
    import pytest as _pytest
    from pymongo import MongoClient
    from pymongo.errors import WriteConcernError
    from pymongo.write_concern import WriteConcern

    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            client["db"]["t"].insert_one({"x": 1})
            db_wc = client.get_database("db", write_concern=WriteConcern(w=50))
            with _pytest.raises(WriteConcernError):
                db_wc.drop_collection("t")
        finally:
            client.close()


def test_unknown_update_operator_rejected_on_empty_collection(tmp_path) -> None:
    """An unknown update modifier is rejected at parse time — even when
    no documents match (mongod validates before matching). Oracle:
    pymongo test_error_code expects code in (9, 10147, 16840, 17009)."""
    from pymongo import MongoClient
    from pymongo.errors import OperationFailure

    from secantus import SecantusDBServer

    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            coll = client["db"]["empty"]  # no documents
            with pytest.raises(OperationFailure) as exc:
                coll.update_many({}, {"$thismodifierdoesntexist": 1})
            assert exc.value.code in (9, 10147, 16840, 17009)
            assert exc.value.details is not None
        finally:
            client.close()


def test_bad_partial_filter_expression_rejected(tmp_path) -> None:
    """createIndexes rejects malformed partialFilterExpression: a
    non-document, an unknown operator, and a logical operator with a
    non-array argument. Oracle: pymongo test_index_filter."""
    from pymongo import MongoClient
    from pymongo.errors import OperationFailure

    from secantus import SecantusDBServer

    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            coll = client["db"]["t"]
            for bad in (5, {"x": {"$asdasd": 3}}, {"$and": 5}):
                with pytest.raises(OperationFailure):
                    coll.create_index("x", partialFilterExpression=bad)
            # A valid partial filter still works.
            assert (
                coll.create_index([("x", 1)], partialFilterExpression={"a": {"$lte": 1.5}}) == "x_1"
            )
        finally:
            client.close()


def test_partial_index_range_implication_is_sound(tmp_path) -> None:
    """A partial index on {a: {$lte: 1.5}} is used only when the query
    GUARANTEES a <= 1.5 (equality a:1, or a:{$lt:1}); a query that could
    match docs outside the partial filter must NOT use it (correctness)
    and must return all matching docs via a full scan."""
    from pymongo import MongoClient

    from secantus import SecantusDBServer

    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            t = client["db"]["t"]
            t.create_index([("x", 1)], partialFilterExpression={"a": {"$lte": 1.5}})
            t.insert_many([{"x": 6, "a": 1}, {"x": 7, "a": 5}])  # a=5 not in index

            # Implying query: uses the partial index, correct result.
            assert t.count_documents({"x": 6, "a": 1}) == 1
            plan = t.find({"x": 6, "a": 1}).explain()["queryPlanner"]["winningPlan"]
            assert plan["inputStage"]["indexName"] == "x_1"
            assert plan["inputStage"]["isPartial"] is True

            # Non-implying query (a < 10 doesn't guarantee a <= 1.5):
            # must NOT use the partial index, must still find the a=5 doc.
            assert t.count_documents({"a": {"$lt": 10}}) == 2
        finally:
            client.close()


def test_exhaust_cursor_streams_all_documents(coll) -> None:
    """CursorType.EXHAUST: the server streams every remaining batch over
    the same socket using OP_MSG moreToCome, without the driver sending a
    getMore per batch. Mirrors pymongo's own test_cursor.test_exhaust."""
    from pymongo import CursorType

    coll.insert_many({"_id": i} for i in range(200))
    cursor = coll.find(cursor_type=CursorType.EXHAUST, batch_size=10)
    got = [d["_id"] for d in cursor]
    assert got == list(range(200))


def test_exhaust_raw_batches_round_trip(server: SecantusDBServer) -> None:
    """find_raw_batches under EXHAUST returns the full result set decoded
    from the streamed raw batches (the exact shape pymongo's gauge asserts)."""
    from bson import decode_all
    from pymongo import CursorType, MongoClient

    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        c = client["db"]["test"]
        c.insert_many({"_id": i} for i in range(200))
        result = b"".join(c.find_raw_batches(cursor_type=CursorType.EXHAUST).to_list())
        assert decode_all(result) == [{"_id": i} for i in range(200)]
    finally:
        client.close()


def test_exhaust_cursor_single_batch_no_stream(coll) -> None:
    """When the whole result fits in firstBatch (cursor id 0), exhaust is a
    no-op: one reply, no moreToCome streaming, correct docs."""
    from pymongo import CursorType

    coll.insert_many({"_id": i} for i in range(5))
    cursor = coll.find(cursor_type=CursorType.EXHAUST)
    assert [d["_id"] for d in cursor] == list(range(5))


def test_exhaust_midstream_getmore_fault_terminates_cleanly(coll, monkeypatch) -> None:
    """If a synthetic mid-stream getMore raises unexpectedly after a
    ``moreToCome`` reply has already gone out, the server must close the
    stream with a final ``moreToCome``-clear reply (empty batch, id 0)
    rather than dropping the connection — which the client would surface as
    "Server ended moreToCome unexpectedly". Regression for the b33 hardening
    in ``SecantusDBServer._stream_exhaust_getmore``."""
    from pymongo import CursorType

    import secantus.server as server_mod

    coll.insert_many({"_id": i} for i in range(5))

    real_dispatch = server_mod.dispatch
    getmore_calls = {"n": 0}

    def faulting_dispatch(body, ctx):
        # Only meddle with getMores against this test's collection; every
        # other command (incl. other servers in this worker) passes through
        # untouched. The first getMore — the client's own exhaustAllowed
        # request that opens the stream — succeeds and produces a non-empty
        # ``moreToCome`` batch. The second getMore is the server's synthetic
        # mid-stream pull; raise there to exercise the fault path.
        if body.get("getMore") is not None and body.get("collection") == coll.name:
            getmore_calls["n"] += 1
            if getmore_calls["n"] >= 2:
                raise RuntimeError("injected mid-stream getMore fault")
        return real_dispatch(body, ctx)

    monkeypatch.setattr(server_mod, "dispatch", faulting_dispatch)

    # batchSize 1 guarantees the cursor stays alive past the first getMore,
    # so the stream emits at least one moreToCome reply before the fault.
    cursor = coll.find(cursor_type=CursorType.EXHAUST, batch_size=1)
    # No MongoUnexpectedServerResponseError: the stream ends cleanly. We got
    # the docs delivered before the fault (find's firstBatch + one streamed
    # batch); the tail is dropped, but the wire stays well-formed.
    got = [d["_id"] for d in cursor]
    assert got == [0, 1]
    assert getmore_calls["n"] >= 2

    # The connection survives — a fresh command on the same client works.
    assert coll.count_documents({}) == 5


def test_json_schema_unique_items_find(coll) -> None:
    """`$jsonSchema` with `uniqueItems: true` over the wire: only docs whose
    array field has all-distinct elements match."""
    coll.insert_many(
        [
            {"_id": 1, "tags": ["a", "b", "c"]},  # unique -> matches
            {"_id": 2, "tags": ["a", "b", "a"]},  # duplicate -> no match
            {"_id": 3, "tags": [1, 1.0]},  # cross-type-equal numeric -> no match
            {"_id": 4, "tags": []},  # empty -> matches
        ]
    )
    schema = {"$jsonSchema": {"properties": {"tags": {"bsonType": "array", "uniqueItems": True}}}}
    got = sorted(d["_id"] for d in coll.find(schema))
    assert got == [1, 4]


def test_json_schema_unique_items_nested_crosstype(coll) -> None:
    """`uniqueItems` value equality bridges cross-type-equal numerics even
    inside nested sub-documents ({a:1} == {a:1.0}), matching real mongod."""
    coll.insert_many(
        [
            {"_id": 1, "arr": [{"a": 1}, {"a": 1.0}]},  # cross-type dup -> no match
            {"_id": 2, "arr": [{"a": 1}, {"a": 2}]},  # distinct -> matches
            {"_id": 3, "arr": [1, 1.0]},  # top-level cross-type dup -> no match
            {"_id": 4, "arr": [1, 2]},  # distinct -> matches
            {"_id": 5, "arr": [{"a": 1}, {"a": 1}]},  # exact dup -> no match
        ]
    )
    schema = {"$jsonSchema": {"properties": {"arr": {"uniqueItems": True}}}}
    got = sorted(d["_id"] for d in coll.find(schema))
    assert got == [2, 4]


def test_unknown_expression_operator_error_codes(coll) -> None:
    """An unrecognized aggregation-expression operator reports mongod's
    context-specific error code: 168 InvalidPipelineOperator inside a query
    ``$expr``; Location31325 inside a ``$project``. Verified against mongod 6.0."""
    import pymongo

    coll.insert_one({"_id": 1, "a": 5})
    with pytest.raises(pymongo.errors.OperationFailure) as expr_exc:
        list(coll.find({"$expr": {"$notreal": [1, 2]}}))
    assert expr_exc.value.code == 168
    assert "Unrecognized expression '$notreal'" in expr_exc.value.details["errmsg"]

    with pytest.raises(pymongo.errors.OperationFailure) as proj_exc:
        list(coll.aggregate([{"$project": {"y": {"$notreal": ["$a"]}}}]))
    assert proj_exc.value.code == 31325
    assert "Unknown expression $notreal" in proj_exc.value.details["errmsg"]


def test_view_reads_resolve_the_pipeline(client: MongoClient) -> None:
    """find / aggregate / count on a view resolve the view's pipeline against its
    base collection (previously they returned nothing). Covers filtering, sort,
    skip, limit, projection, and a view defined on another view."""
    db = client["view_reads"]
    db.src.insert_many([{"_id": i, "a": i % 3, "v": i} for i in range(9)])
    db.command("create", "vw", viewOn="src", pipeline=[{"$match": {"a": 1}}])

    # find over the view (a == 1 → _ids 1, 4, 7)
    assert sorted(d["_id"] for d in db.vw.find({})) == [1, 4, 7]
    assert sorted(d["_id"] for d in db.vw.find({"v": {"$gt": 3}})) == [4, 7]
    assert [d["_id"] for d in db.vw.find({}).sort("_id", -1).limit(2)] == [7, 4]
    assert [d["_id"] for d in db.vw.find({}).sort("_id", 1).skip(1)] == [4, 7]
    assert db.vw.find_one({"_id": 4}, {"v": 1, "_id": 0}) == {"v": 4}

    # aggregate + count over the view
    assert [d["_id"] for d in db.vw.aggregate([{"$sort": {"_id": 1}}])] == [1, 4, 7]
    assert db.vw.count_documents({}) == 3
    assert db.vw.count_documents({"v": {"$gt": 3}}) == 2

    # a view defined on another view resolves recursively
    db.command("create", "vw2", viewOn="vw", pipeline=[{"$match": {"v": {"$gt": 3}}}])
    assert sorted(d["_id"] for d in db.vw2.find({})) == [4, 7]
    assert db.vw2.count_documents({}) == 2


def test_push_addtoset_each_modifiers(coll) -> None:
    """`$push` / `$addToSet` `$each` (with `$position` / `$slice` / `$sort`) append
    multiple elements, previously the `$each` doc was stored as a single element."""
    coll.insert_one({"_id": 1, "a": [3, 1, 2]})
    coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [5, 4]}}})
    assert coll.find_one({"_id": 1})["a"] == [3, 1, 2, 5, 4]

    coll.update_one({"_id": 1}, {"$set": {"a": [3, 1, 2]}})
    coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [5, 4], "$sort": 1}}})
    assert coll.find_one({"_id": 1})["a"] == [1, 2, 3, 4, 5]

    coll.update_one({"_id": 1}, {"$set": {"a": [1, 2, 3]}})
    coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [9], "$slice": -2}}})
    assert coll.find_one({"_id": 1})["a"] == [3, 9]

    coll.update_one({"_id": 1}, {"$set": {"a": [1, 2, 3]}})
    coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [9, 8], "$position": 1}}})
    assert coll.find_one({"_id": 1})["a"] == [1, 9, 8, 2, 3]

    coll.update_one({"_id": 1}, {"$set": {"a": [1, 2]}})
    coll.update_one({"_id": 1}, {"$addToSet": {"a": {"$each": [2, 3, 3, 4]}}})
    assert coll.find_one({"_id": 1})["a"] == [1, 2, 3, 4]

    # plain (non-$each) push/addToSet unchanged
    coll.update_one({"_id": 1}, {"$set": {"a": [1]}})
    coll.update_one({"_id": 1}, {"$push": {"a": 7}})
    assert coll.find_one({"_id": 1})["a"] == [1, 7]


def test_update_bit_multiple_ops(coll) -> None:
    """`$bit` applies multiple operations to a field in order (mongod semantics)."""
    coll.insert_one({"_id": 1, "n": 0b1100})
    coll.update_one({"_id": 1}, {"$bit": {"n": {"and": 0b1010, "or": 0b0001}}})
    assert coll.find_one({"_id": 1})["n"] == 0b1001  # (0b1100 & 0b1010) | 0b0001


def test_update_min_max_cross_type(coll) -> None:
    """`$min`/`$max` compare by BSON cross-type order (no server-side crash on a
    string-vs-number compare); a missing field is set, explicit null is a value."""
    coll.insert_one({"_id": 1, "a": 5})
    coll.update_one({"_id": 1}, {"$max": {"a": "str"}})  # string > number
    assert coll.find_one({"_id": 1})["a"] == "str"
    coll.update_one({"_id": 1}, {"$set": {"a": 5}})
    coll.update_one({"_id": 1}, {"$min": {"a": "str"}})  # number < string -> keep 5
    assert coll.find_one({"_id": 1})["a"] == 5
    # Explicit null vs number.
    coll.update_one({"_id": 1}, {"$set": {"a": None}})
    coll.update_one({"_id": 1}, {"$min": {"a": 9}})  # null < 9 -> keep null
    assert coll.find_one({"_id": 1})["a"] is None


def test_pull_predicate_and_pullall(coll) -> None:
    """`$pull` with a query predicate / sub-document criterion, and `$pullAll`."""
    coll.insert_one({"_id": 1, "a": [1, 5, 10, 15]})
    coll.update_one({"_id": 1}, {"$pull": {"a": {"$gte": 10}}})
    assert coll.find_one({"_id": 1})["a"] == [1, 5]

    coll.update_one(
        {"_id": 1},
        {"$set": {"a": [{"x": 1, "y": "a"}, {"x": 5, "y": "b"}, {"x": 9, "y": "c"}]}},
    )
    coll.update_one({"_id": 1}, {"$pull": {"a": {"x": {"$gte": 5}}}})
    assert coll.find_one({"_id": 1})["a"] == [{"x": 1, "y": "a"}]

    # query eq: bool is type-distinct from int (keeps True); 1 == 1.0 numerically.
    coll.update_one({"_id": 1}, {"$set": {"a": [1, True, 2]}})
    coll.update_one({"_id": 1}, {"$pull": {"a": 1}})
    assert coll.find_one({"_id": 1})["a"] == [True, 2]

    # $pullAll removes every listed value by literal equality.
    coll.update_one({"_id": 1}, {"$set": {"a": [1, 2, 3, 2, 1]}})
    coll.update_one({"_id": 1}, {"$pullAll": {"a": [1, 2]}})
    assert coll.find_one({"_id": 1})["a"] == [3]


def test_json_schema_keyword_validation_wire(coll) -> None:
    """$jsonSchema keyword validation over the wire, verbatim from a mongod 7.0
    probe: metadata keywords accepted; unsupported keywords 9 FailedToParse;
    unknown keywords 9 (nested schemas included); draft-4 exclusive bounds and
    multipleOf / tuple-items semantics."""
    import pymongo

    coll.insert_many([{"_id": 1, "n": 6, "arr": [1, "x"]}, {"_id": 2, "n": 7.5, "arr": [1]}])

    assert len(list(coll.find({"$jsonSchema": {"title": "t", "description": "d"}}))) == 2
    got = [
        d["_id"]
        for d in coll.find(
            {"$jsonSchema": {"properties": {"n": {"minimum": 6, "exclusiveMinimum": True}}}}
        )
    ]
    assert got == [2]
    assert [
        d["_id"] for d in coll.find({"$jsonSchema": {"properties": {"n": {"multipleOf": 2.5}}}})
    ] == [2]
    assert [
        d["_id"]
        for d in coll.find(
            {
                "$jsonSchema": {
                    "properties": {
                        "arr": {"items": [{"bsonType": "int"}], "additionalItems": False}
                    }
                }
            }
        )
    ] == [2]

    for schema, code, frag in [
        ({"$ref": "#/x"}, 9, "not currently supported"),
        ({"notakeyword": 1}, 9, "Unknown $jsonSchema keyword: notakeyword"),
        ({"properties": {"n": {"notakeyword": 1}}}, 9, "Unknown $jsonSchema keyword"),
        ({"minimum": 5, "exclusiveMinimum": 6}, 14, "must be a boolean"),
        ({"exclusiveMinimum": True}, 9, "must be a present if"),
        ({"multipleOf": 0}, 9, "positive value"),
        ({"title": 5}, 14, "must be of type string"),
    ]:
        with pytest.raises(pymongo.errors.OperationFailure) as exc:
            list(coll.find({"$jsonSchema": schema}))
        assert exc.value.code == code, schema
        assert frag in exc.value.details["errmsg"], schema


def test_median_and_percentile_over_the_wire(coll) -> None:
    """$median / $percentile group accumulators + expression forms, matching a
    mongod 7.0.12 probe (discrete percentile, doubles out, verbatim errors)."""
    import pymongo

    coll.insert_many([{"x": v} for v in [10, 20, 30, 40]])
    r = list(
        coll.aggregate(
            [
                {
                    "$group": {
                        "_id": None,
                        "med": {"$median": {"input": "$x", "method": "approximate"}},
                        "pct": {
                            "$percentile": {
                                "input": "$x",
                                "p": [0.1, 0.5, 0.75, 0.9],
                                "method": "approximate",
                            }
                        },
                    }
                }
            ]
        )
    )[0]
    assert r["med"] == 20.0
    assert r["pct"] == [10.0, 20.0, 30.0, 40.0]

    r = list(
        coll.aggregate(
            [
                {"$limit": 1},
                {
                    "$project": {
                        "m": {"$median": {"input": [1, 2, 3, 4], "method": "approximate"}},
                    }
                },
            ]
        )
    )[0]
    assert r["m"] == 2.0

    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(coll.aggregate([{"$group": {"_id": None, "m": {"$median": {"input": "$x"}}}}]))
    assert exc.value.code == 40414
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(
            coll.aggregate(
                [
                    {
                        "$group": {
                            "_id": None,
                            "p": {
                                "$percentile": {
                                    "input": "$x",
                                    "p": [1.5],
                                    "method": "approximate",
                                }
                            },
                        }
                    }
                ]
            )
        )
    assert exc.value.code == 7750303


def test_range_orders_embedded_documents(coll) -> None:
    """$gt/$lt over an embedded-document bound orders documents field-by-field
    (mongod 7.0.12); a document-valued field never matches a scalar bound.
    Regression for a bug where both servers returned nothing for a doc bound."""
    coll.insert_many(
        [
            {"_id": 1, "a": {"x": 2}},
            {"_id": 2, "a": {"x": 0}},
            {"_id": 3, "a": {"x": 1}},
            {"_id": 4, "a": {"x": 1, "y": 9}},
            {"_id": 5, "a": {"y": 1}},
            {"_id": 6, "a": 5},  # scalar — different bracket
        ]
    )
    assert sorted(d["_id"] for d in coll.find({"a": {"$gt": {"x": 1}}})) == [1, 4, 5]
    assert sorted(d["_id"] for d in coll.find({"a": {"$gte": {"x": 1}}})) == [1, 3, 4, 5]
    assert sorted(d["_id"] for d in coll.find({"a": {"$lt": {"x": 1}}})) == [2]
    # A scalar field never matches a document bound.
    assert 6 not in [d["_id"] for d in coll.find({"a": {"$gt": {"x": 1}}})]


def test_query_mod_truncation_and_bool(coll) -> None:
    """$mod over the wire: double values (and the divisor) truncate toward
    zero, bool is excluded, C-style modulo. Regression for a bug where the Rust
    server errored on a double-valued field and both servers matched bool."""
    coll.insert_many(
        [
            {"_id": 1, "a": 5},
            {"_id": 2, "a": 5.0},
            {"_id": 3, "a": True},
            {"_id": 4, "a": 5.5},
            {"_id": 5, "a": -5},
            {"_id": 6, "a": 4.9},
        ]
    )
    assert sorted(d["_id"] for d in coll.find({"a": {"$mod": [2, 1]}})) == [1, 2, 4]
    assert sorted(d["_id"] for d in coll.find({"a": {"$mod": [2, 0]}})) == [6]
    assert sorted(d["_id"] for d in coll.find({"a": {"$mod": [2.5, 0]}})) == [6]


def test_query_size_validation(coll) -> None:
    """$size over the wire: an integer-valued float is accepted; a negative /
    non-integer / string / bool argument is a parse error (code 2), not a
    silent empty result."""
    import pymongo

    coll.insert_many([{"_id": 1, "a": [1, 2]}, {"_id": 2, "a": [1]}])
    assert [d["_id"] for d in coll.find({"a": {"$size": 2.0}})] == [1]
    for bad in (-1, 2.5, "2", True):
        with pytest.raises(pymongo.errors.OperationFailure) as exc:
            list(coll.find({"a": {"$size": bad}}))
        assert exc.value.code == 2, bad


def test_update_inc_mul_non_numeric_operand(coll) -> None:
    """$inc / $mul by a non-number is rejected (code 14) over the wire on the
    Python server, matching mongod — not silently computed."""
    import pymongo

    coll.insert_one({"_id": 1, "n": 5})
    for op in ("$inc", "$mul"):
        for operand in (True, "x", None):
            with pytest.raises(pymongo.errors.OperationFailure) as exc:
                coll.update_one({"_id": 1}, {op: {"n": operand}})
            assert exc.value.code == 14, (op, operand)
    # The document is untouched by the rejected updates.
    assert coll.find_one({"_id": 1})["n"] == 5
    # A valid $inc still applies.
    coll.update_one({"_id": 1}, {"$inc": {"n": 3}})
    assert coll.find_one({"_id": 1})["n"] == 8


def test_aggregation_expr_bool_argument_rejected(coll) -> None:
    """A bool where an aggregation operator expects a numeric (int) argument is
    a parse error in mongod (bool is not a number) — SecantusDB surfaces the
    exact error code over the wire. Three-way mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    for expr, code in [
        ({"$round": [1.5, True]}, 16004),
        ({"$trunc": [1.5, True]}, 16004),
        ({"$arrayElemAt": [[10, 20, 30], True]}, 28690),
        ({"$slice": [[1, 2, 3, 4], True]}, 28725),
        ({"$slice": [[1, 2, 3, 4], 1, True]}, 28727),
        ({"$sortArray": {"input": [3, 1, 2], "sortBy": True}}, 2942507),
        ({"$substrCP": ["hello", True, 2]}, 34450),
        ({"$substrCP": ["hello", 1, True]}, 34452),
        ({"$substrBytes": ["hello", True, 2]}, 16034),
        ({"$substrBytes": ["hello", 1, True]}, 16035),
        ({"$substr": ["hello", True, 2]}, 16034),  # $substr aliases $substrBytes
        ({"$substr": ["hello", 1, True]}, 16035),
        ({"$range": [True, 5]}, 34443),
        ({"$range": [0, True]}, 34445),
        ({"$range": [0, 5, True]}, 34447),
        ({"$indexOfArray": [[1, 2, 3], 2, True]}, 40096),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == code, expr

    # An int argument still computes.
    out = list(coll.aggregate([{"$project": {"r": {"$arrayElemAt": [[10, 20, 30], 1]}, "_id": 0}}]))
    assert out == [{"r": 20}]


def test_aggregation_whole_double_index_accepted(coll) -> None:
    """mongod accepts a whole-number double where an int index is expected and
    rejects a fractional double. Both hold over the wire. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    got = list(
        coll.aggregate([{"$project": {"r": {"$arrayElemAt": [[10, 20, 30], 2.0]}, "_id": 0}}])
    )
    assert got == [{"r": 30}]
    got = list(
        coll.aggregate([{"$project": {"r": {"$slice": [[1, 2, 3, 4], 1.0, 2.0]}, "_id": 0}}])
    )
    assert got == [{"r": [2, 3]}]

    for expr, code in [
        ({"$arrayElemAt": [[10, 20, 30], 2.7]}, 28691),
        ({"$slice": [[1, 2, 3, 4], 2.7]}, 28726),
        ({"$slice": [[1, 2, 3, 4], 1, 1.7]}, 28728),
        ({"$indexOfArray": [[1, 2, 3], 2, 0.7]}, 40096),
        ({"$substrCP": ["hello", 1.7, 2]}, 34451),
        ({"$range": [0, 5.7]}, 34446),
        ({"$round": [3.14159, 2.7]}, 51082),
        ({"$trunc": [3.14159, 2.7]}, 51082),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == code, expr


def test_split_argument_validation_via_pymongo(coll) -> None:
    """$split: empty separator 40087, non-string first/second 40085/40086, wrong
    arg count 16020; a null string / separator yields null. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    for expr, code in [
        ({"$split": ["a,b", ""]}, 40087),
        ({"$split": [5, ","]}, 40085),
        ({"$split": ["a,b", 5]}, 40086),
        ({"$split": ["a,b"]}, 16020),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == code, expr
    got = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "a": {"$split": ["a,b,c", ","]},
                        "n": {"$split": [None, ","]},
                    }
                }
            ]
        )
    )
    assert got == [{"a": ["a", "b", "c"], "n": None}]


def test_substr_bytes_split_utf8_rejected(coll) -> None:
    """$substrBytes rejects a range that splits a UTF-8 character (mongod codes
    28656 start / 28657 end) over the wire. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    for expr, code in [
        ({"$substrBytes": ["héllo", 0, 2]}, 28657),
        ({"$substrBytes": ["héllo", 2, 3]}, 28656),
        ({"$substr": ["héllo", 0, 2]}, 28657),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == code, expr
    # Clean boundary still computes.
    got = list(coll.aggregate([{"$project": {"r": {"$substrBytes": ["héllo", 0, 3]}, "_id": 0}}]))
    assert got == [{"r": "hé"}]


def test_substr_negative_index_rejected(coll) -> None:
    """$substr* reject a negative start (50752 / 34455); $substrCP also rejects a
    negative length (34454), while $substrBytes treats it as "to end". mongod
    7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    for expr, code in [
        ({"$substrBytes": ["abcde", -1, 2]}, 50752),
        ({"$substrCP": ["abcde", -1, 2]}, 34455),
        ({"$substrCP": ["abcde", 1, -1]}, 34454),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == code, expr
    got = list(coll.aggregate([{"$project": {"r": {"$substrBytes": ["abcde", 1, -1]}, "_id": 0}}]))
    assert got == [{"r": "bcde"}]


def test_substr_bytes_truncates_double_index(coll) -> None:
    """$substrBytes truncates a double index toward zero (mongod-faithful),
    unlike $substrCP which rejects a fractional double. mongod 7.0.12-verified."""
    coll.insert_one({"_id": 1})
    for expr, want in [
        ({"$substrBytes": ["abcde", 1.7, 2]}, "bc"),
        ({"$substrBytes": ["abcde", 0.9, 3]}, "abc"),
        ({"$substrBytes": ["abcde", 1, 2.9]}, "bc"),
    ]:
        got = list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert got == [{"r": want}], expr


def test_limit_skip_numeric_arg_validation(coll) -> None:
    """$limit / $skip accept a whole-number double but reject bool / fractional /
    negative (5107201 / 5107200), and $limit rejects zero (15958), over the wire.
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": i} for i in range(10)])
    assert len(list(coll.aggregate([{"$limit": 2.0}]))) == 2
    assert len(list(coll.aggregate([{"$skip": 3.0}]))) == 7
    for pipe, code in [
        ([{"$limit": 2.7}], 5107201),
        ([{"$limit": True}], 5107201),
        ([{"$limit": 0}], 15958),
        ([{"$skip": 3.7}], 5107200),
        ([{"$skip": -1}], 5107200),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate(pipe))
        assert exc.value.code == code, pipe


def test_sample_size_validation(coll) -> None:
    """$sample rejects a bool size (28746) and a negative size (28747) over the
    wire, and truncates a fractional size. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": i} for i in range(10)])
    assert len(list(coll.aggregate([{"$sample": {"size": 3}}]))) == 3
    assert len(list(coll.aggregate([{"$sample": {"size": 2.7}}]))) == 2
    for size, code in [(True, 28746), (-1, 28747)]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$sample": {"size": size}}]))
        assert exc.value.code == code, size


def test_bits_numeric_arg_validation(coll) -> None:
    """$bits* accept a whole-double mask/position and reject fractional / negative
    / bool with mongod's codes (position 2, non-array mask 9) over the wire.
    mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "n": 6})
    assert coll.count_documents({"n": {"$bitsAllSet": 6.0}}) == 1
    assert coll.count_documents({"n": {"$bitsAllSet": [1.0, 2.0]}}) == 1
    for query, code in [
        ({"n": {"$bitsAllSet": 2.5}}, 9),
        ({"n": {"$bitsAllSet": -1}}, 9),
        ({"n": {"$bitsAllSet": [1.5]}}, 2),
        ({"n": {"$bitsAllSet": [-1]}}, 2),
    ]:
        with pytest.raises(OperationFailure) as exc:
            coll.count_documents(query)
        assert exc.value.code == code, query


def test_array_operators_reject_non_array_via_pymongo(coll) -> None:
    """$first/$last (28689), $reverseArray (34435), $concatArrays (28664),
    $slice (28724), $map (16883), $filter (28651), $reduce (40080) reject a
    non-array input; a null / missing input yields null. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    for expr, code in [
        ({"$first": 5}, 28689),
        ({"$reverseArray": 5}, 34435),
        ({"$concatArrays": [[1], 5]}, 28664),
        ({"$slice": [5, 2]}, 28724),
        ({"$map": {"input": 5, "in": "$$this"}}, 16883),
        ({"$filter": {"input": 5, "cond": True}}, 28651),
        ({"$reduce": {"input": 5, "initialValue": 0, "in": "$$value"}}, 40080),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == code, expr
    out = list(coll.aggregate([{"$project": {"_id": 0, "n": {"$first": "$gone"}}}]))
    assert out == [{"n": None}]


def test_index_of_start_end_validation_via_pymongo(coll) -> None:
    """$indexOfBytes/$indexOfCP: a fractional / bool / non-numeric start or end is
    40096, a negative one is 40097; a whole double is accepted. mongod
    7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    for op in ("$indexOfBytes", "$indexOfCP"):
        for bad, code in [(2.5, 40096), (True, 40096), ("x", 40096), (-1, 40097)]:
            with pytest.raises(OperationFailure) as exc:
                list(coll.aggregate([{"$project": {"r": {op: ["abcabc", "b", bad]}, "_id": 0}}]))
            assert exc.value.code == code, (op, bad)
    got = list(
        coll.aggregate([{"$project": {"_id": 0, "i": {"$indexOfBytes": ["abcabc", "b", 2.0]}}}])
    )
    assert got == [{"i": 4}]


def test_trim_argument_validation_via_pymongo(coll) -> None:
    """$trim/$ltrim/$rtrim: non-string input -> 50699, non-string chars -> 50700;
    a null chars yields null. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    for op in ("$trim", "$ltrim", "$rtrim"):
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": {op: {"input": 5}}, "_id": 0}}]))
        assert exc.value.code == 50699, op
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": {op: {"input": "x", "chars": 5}}, "_id": 0}}]))
        assert exc.value.code == 50700, op
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "t": {"$trim": {"input": "--x--", "chars": "-"}},
                        "n": {"$trim": {"input": "x", "chars": None}},
                    }
                }
            ]
        )
    )
    assert out == [{"t": "x", "n": None}]


def test_concat_type_validation_via_pymongo(coll) -> None:
    """$concat: a non-string operand is 16702; a null / missing operand yields
    null. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "s": "b"})
    for expr in ({"$concat": ["a", 5]}, {"$concat": ["a", True]}, {"$concat": [["x"]]}):
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == 16702, expr
    out = list(
        coll.aggregate(
            [
                {
                    "$project": {
                        "_id": 0,
                        "ok": {"$concat": ["a", "$s"]},
                        "n": {"$concat": ["a", None]},
                        "m": {"$concat": ["a", "$gone"]},
                    }
                }
            ]
        )
    )
    assert out == [{"ok": "ab", "n": None, "m": None}]


def test_pow_domain_validation(coll) -> None:
    """$pow: negative base + fractional exponent returns NaN (not a server crash),
    and bad operands raise mongod's codes over the wire. mongod 7.0.12-verified."""
    import math

    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1})
    got = list(coll.aggregate([{"$project": {"r": {"$pow": [-2, 0.5]}, "_id": 0}}]))
    assert len(got) == 1 and math.isnan(got[0]["r"])  # regression: used to crash BSON encode
    assert list(coll.aggregate([{"$project": {"r": {"$pow": [-2, 3]}, "_id": 0}}]))[0]["r"] == -8
    for expr, code in [
        ({"$pow": ["x", 2]}, 28762),
        ({"$pow": [2, True]}, 28763),
        ({"$pow": [0, -1]}, 28764),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        assert exc.value.code == code, expr


def test_unary_math_rejects_non_numeric_via_pymongo(coll) -> None:
    """$abs/$ceil/$floor/$sqrt/$exp/$ln/$log10 reject a string/bool operand
    (28765), $round/$trunc (51081); null passes through. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "s": "x", "n": 4.0})
    for op in ("$abs", "$ceil", "$floor", "$sqrt", "$exp", "$ln", "$log10"):
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": {op: "$s"}}}]))
        assert exc.value.code == 28765, op
    for op in ("$round", "$trunc"):
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$project": {"r": {op: ["$s", 0]}}}]))
        assert exc.value.code == 51081, op
    # $log: a non-numeric argument (28756) / base (28757).
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {"r": {"$log": ["$s", 2]}}}]))
    assert exc.value.code == 28756
    with pytest.raises(OperationFailure) as exc:
        list(coll.aggregate([{"$project": {"r": {"$log": [8, "$s"]}}}]))
    assert exc.value.code == 28757
    # A whole-double operand still computes; a null field yields null.
    got = list(
        coll.aggregate([{"$project": {"_id": 0, "a": {"$abs": "$n"}, "m": {"$abs": "$gone"}}}])
    )
    assert got == [{"a": 4.0, "m": None}]


def test_gte_lte_null_and_exists_truthiness(coll) -> None:
    """$gte/$lte: null match null + missing; $exists uses mongod truthiness.
    mongod 7.0.12-verified over the wire."""
    coll.insert_many([{"_id": 1, "f": None}, {"_id": 2, "f": 5}, {"_id": 3}])

    def ids(q):
        return sorted(d["_id"] for d in coll.find(q))

    assert ids({"f": {"$gte": None}}) == [1, 3]
    assert ids({"f": {"$lte": None}}) == [1, 3]
    assert ids({"f": {"$gt": None}}) == []
    assert ids({"f": {"$exists": ""}}) == [1, 2]
    assert ids({"f": {"$exists": []}}) == [1, 2]
    assert ids({"f": {"$exists": 0}}) == [3]


def test_rename_validation_no_corruption(coll) -> None:
    """$rename rejects an array-element / same-field / empty / non-string spec
    (was silent data corruption / a leaked exception) over the wire, and a valid
    rename still applies. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_one({"_id": 1, "a": 5, "arr": [1, 2, 3]})
    for upd in (
        {"$rename": {"a": "a"}},
        {"$rename": {"arr.0": "x"}},
        {"$rename": {"a": "arr.0"}},
        {"$rename": {"a": ""}},
        {"$rename": {"a": 5}},
    ):
        with pytest.raises(OperationFailure):
            coll.update_one({"_id": 1}, upd)
    assert coll.find_one({"_id": 1})["arr"] == [1, 2, 3]  # not corrupted
    coll.update_one({"_id": 1}, {"$rename": {"a": "z"}})
    doc = coll.find_one({"_id": 1})
    assert doc.get("z") == 5 and "a" not in doc


def test_bucket_validation_no_data_loss(coll) -> None:
    """$bucket errors on an out-of-range value with no default (was silent data
    loss) and validates its spec, over the wire. mongod 7.0.12-verified."""
    from pymongo.errors import OperationFailure

    coll.insert_many([{"_id": i, "v": i} for i in range(6)])
    r = list(coll.aggregate([{"$bucket": {"groupBy": "$v", "boundaries": [0, 3, 6]}}]))
    assert [(b["_id"], b["count"]) for b in r] == [(0, 3), (3, 3)]
    for spec, code in [
        ({"groupBy": "$v", "boundaries": [0, 3]}, 7158303),
        ({"groupBy": "$v", "boundaries": [0, 5, 2]}, 40194),
        ({"boundaries": [0, 6]}, 40198),
    ]:
        with pytest.raises(OperationFailure) as exc:
            list(coll.aggregate([{"$bucket": spec}]))
        assert exc.value.code == code, spec


def test_failgetmore_after_cursor_checkout_stamps_resumable_label(server) -> None:
    """``failGetMoreAfterCursorCheckout`` + a resumable code resumes the stream.

    mongod injects this failpoint *inside* the change-stream getMore path,
    where a resumable error code comes back stamped
    ``ResumableChangeStreamError`` — that label is the whole reason a driver
    resumes instead of surfacing the error. Ignoring the failpoint (the
    previous behaviour) meant the getMore simply succeeded and no resume ever
    happened; libmongoc's ``change-streams-resume-errorLabels`` saw 2 commands
    where the spec requires 3.

    Asserting on COMMAND MONITORING, not on the delivered event, is the whole
    point: the event arrives either way (with the failpoint ignored the first
    getMore just works), so a test that only checks the event passes whether or
    not the fix is present. The resume is only observable as a *second*
    ``aggregate`` on the wire.
    """
    from pymongo import MongoClient, monitoring

    started: list[str] = []

    class _Listener(monitoring.CommandListener):
        def started(self, event: monitoring.CommandStartedEvent) -> None:
            started.append(event.command_name)

        def succeeded(self, event: object) -> None:
            pass

        def failed(self, event: object) -> None:
            pass

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=5000, event_listeners=[_Listener()])
    try:
        db = mc["resume_label_db"]
        db.create_collection("c")
        mc.admin.command(
            {
                "configureFailPoint": "failGetMoreAfterCursorCheckout",
                "mode": {"times": 1},
                "data": {"errorCode": 6, "closeConnection": False},  # HostUnreachable
            }
        )
        started.clear()
        with db["c"].watch() as stream:
            db["c"].insert_one({"x": 1})
            change = stream.next()
            assert change["operationType"] == "insert"
        # The resume is the second aggregate. Without the label the driver
        # would surface the error instead, leaving exactly one.
        assert started.count("aggregate") >= 2, (
            f"expected a resume (2nd aggregate); saw commands: {started}"
        )
    finally:
        mc.close()


def test_plain_failcommand_does_not_stamp_resumable_label(client) -> None:
    """Plain ``failCommand`` must NOT add the label — the spec pins the split.

    ``failCommand`` short-circuits before mongod's change-stream machinery, so
    it carries only the labels the failpoint itself named. Same error code, a
    different failpoint, deliberately the opposite outcome: the error must
    reach the client rather than being resumed away. Stamping the label
    unconditionally would silently swallow errors a test expects to see.
    """
    from pymongo.errors import PyMongoError

    db = client["no_resume_label_db"]
    db.create_collection("c")

    with db["c"].watch() as stream:
        db["c"].insert_one({"x": 1})
        assert stream.next()["operationType"] == "insert"
        client.admin.command(
            {
                "configureFailPoint": "failCommand",
                "mode": {"times": 1},
                "data": {"failCommands": ["getMore"], "errorCode": 6},
            }
        )
        db["c"].insert_one({"x": 2})
        with pytest.raises(PyMongoError):
            stream.next()
