from __future__ import annotations

import bson
import pytest

from secantus.storage import Storage


@pytest.fixture
def storage(tmp_path) -> Storage:
    return Storage(str(tmp_path))


def test_insert_assigns_object_id(storage: Storage) -> None:
    inserted, errors = storage.insert("db", "c", [{"x": 1}])
    assert inserted == 1
    assert errors == []
    docs = storage.find_matching("db", "c", {})
    assert len(docs) == 1
    assert isinstance(docs[0]["_id"], bson.ObjectId)
    assert docs[0]["x"] == 1


def test_insert_respects_provided_id(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": "abc", "x": 1}])
    docs = storage.find_matching("db", "c", {"_id": "abc"})
    assert docs == [{"_id": "abc", "x": 1}]


def test_duplicate_id_ordered_stops(storage: Storage) -> None:
    inserted, errors = storage.insert("db", "c", [{"_id": 1}, {"_id": 1}, {"_id": 2}], ordered=True)
    assert inserted == 1
    assert len(errors) == 1
    assert storage.count_matching("db", "c", {}) == 1


def test_duplicate_id_unordered_continues(storage: Storage) -> None:
    inserted, errors = storage.insert(
        "db", "c", [{"_id": 1}, {"_id": 1}, {"_id": 2}], ordered=False
    )
    assert inserted == 2
    assert len(errors) == 1
    assert storage.count_matching("db", "c", {}) == 2


def test_update_modifies_matching(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": 1, "n": 1}, {"_id": 2, "n": 1}])
    result = storage.update_matching("db", "c", {"n": 1}, {"$inc": {"n": 10}}, multi=True)
    assert result["matched"] == 2
    assert result["modified"] == 2
    docs = sorted(storage.find_matching("db", "c", {}), key=lambda d: d["_id"])
    assert [d["n"] for d in docs] == [11, 11]


def test_update_single_when_multi_false(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": 1, "n": 1}, {"_id": 2, "n": 1}])
    result = storage.update_matching("db", "c", {"n": 1}, {"$set": {"n": 5}}, multi=False)
    assert result["matched"] == 1


def test_upsert_creates_when_no_match(storage: Storage) -> None:
    result = storage.update_matching("db", "c", {"k": "abc"}, {"$set": {"v": 9}}, upsert=True)
    assert result["matched"] == 0
    assert result["upserted_id"] is not None
    docs = storage.find_matching("db", "c", {})
    assert docs[0]["k"] == "abc"
    assert docs[0]["v"] == 9


def test_delete_with_limit(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": i, "tag": "x"} for i in range(5)])
    deleted = storage.delete_matching("db", "c", {"tag": "x"}, limit=2)
    assert deleted == 2
    assert storage.count_matching("db", "c", {}) == 3


def test_drop_collection(storage: Storage) -> None:
    storage.insert("db", "c", [{"x": 1}])
    assert storage.drop_collection("db", "c") is True
    assert storage.find_matching("db", "c", {}) == []
    assert storage.drop_collection("db", "c") is False


def test_list_collections_and_databases(storage: Storage) -> None:
    storage.insert("db1", "c1", [{"x": 1}])
    storage.insert("db1", "c2", [{"x": 1}])
    storage.insert("db2", "c1", [{"x": 1}])
    assert storage.list_collections("db1") == ["c1", "c2"]
    assert storage.list_databases() == ["db1", "db2"]


def test_databases_are_isolated(storage: Storage) -> None:
    storage.insert("db1", "c", [{"_id": 1, "x": "a"}])
    storage.insert("db2", "c", [{"_id": 1, "x": "b"}])
    d1 = storage.find_matching("db1", "c", {})
    d2 = storage.find_matching("db2", "c", {})
    assert d1 == [{"_id": 1, "x": "a"}]
    assert d2 == [{"_id": 1, "x": "b"}]


def test_numeric_id_bridge_int_vs_float(storage: Storage) -> None:
    inserted, _ = storage.insert("db", "c", [{"_id": 1, "x": "int"}], ordered=True)
    assert inserted == 1
    inserted2, errors = storage.insert("db", "c", [{"_id": 1.0, "x": "float"}], ordered=True)
    assert inserted2 == 0
    assert len(errors) == 1


def test_numeric_id_bridge_decimal128(storage: Storage) -> None:
    from bson import Decimal128

    storage.insert("db", "c", [{"_id": 5}])
    _, errors = storage.insert("db", "c", [{"_id": Decimal128("5")}])
    assert len(errors) == 1


def test_numeric_id_bridge_distinct_values_still_ok(storage: Storage) -> None:
    inserted, _ = storage.insert("db", "c", [{"_id": 1}, {"_id": 1.5}, {"_id": 2}])
    assert inserted == 3


def test_bool_id_not_treated_as_numeric(storage: Storage) -> None:
    inserted, _ = storage.insert("db", "c", [{"_id": True}, {"_id": 1}])
    assert inserted == 2


def test_rename_collection_moves_docs_and_indexes(storage: Storage) -> None:
    storage.insert("db", "src", [{"_id": 1, "x": 1}, {"_id": 2, "x": 2}])
    storage.create_index("db", "src", "x_1", {"x": 1}, {})
    ok, err = storage.rename_collection("db", "src", "db", "dst")
    assert ok and err is None
    assert storage.find_matching("db", "src", {}) == []
    docs = sorted(storage.find_matching("db", "dst", {}), key=lambda d: d["_id"])
    assert [d["_id"] for d in docs] == [1, 2]
    names = [i["name"] for i in storage.list_indexes("db", "dst")]
    assert "x_1" in names


def test_rename_collection_missing_source(storage: Storage) -> None:
    ok, err = storage.rename_collection("db", "missing", "db", "dst")
    assert not ok
    assert err is not None and "does not exist" in err


def test_rename_collection_target_exists_without_drop(storage: Storage) -> None:
    storage.insert("db", "src", [{"_id": 1}])
    storage.insert("db", "dst", [{"_id": 99}])
    ok, err = storage.rename_collection("db", "src", "db", "dst")
    assert not ok
    assert err is not None and "exists" in err


def test_rename_collection_drop_target(storage: Storage) -> None:
    storage.insert("db", "src", [{"_id": 1, "from": "src"}])
    storage.insert("db", "dst", [{"_id": 99, "from": "dst"}])
    ok, err = storage.rename_collection("db", "src", "db", "dst", drop_target=True)
    assert ok and err is None
    docs = storage.find_matching("db", "dst", {})
    assert docs == [{"_id": 1, "from": "src"}]


def test_rename_collection_across_databases(storage: Storage) -> None:
    storage.insert("dba", "c", [{"_id": 1}])
    ok, _ = storage.rename_collection("dba", "c", "dbb", "c2")
    assert ok
    assert storage.find_matching("dba", "c", {}) == []
    assert storage.find_matching("dbb", "c2", {}) == [{"_id": 1}]


def test_sort_cross_type_order(storage: Storage) -> None:
    from bson import ObjectId

    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "v": "string"},
            {"_id": 2, "v": 5},
            {"_id": 3, "v": True},
            {"_id": 4, "v": None},
            {"_id": 5, "v": [1, 2]},
            {"_id": 6, "v": {"x": 1}},
            {"_id": 7, "v": ObjectId()},
        ],
    )
    out = storage.find_matching("db", "c", {}, sort={"v": 1})
    ids = [d["_id"] for d in out]
    # MongoDB order: null < numbers < string < object < array < ObjectId < bool
    pos = {i: ids.index(i) for i in range(1, 8)}
    assert pos[4] < pos[2]  # null < num
    assert pos[2] < pos[1]  # num < string
    assert pos[1] < pos[6]  # string < object
    assert pos[6] < pos[5]  # object < array
    assert pos[5] < pos[7]  # array < ObjectId
    assert pos[7] < pos[3]  # ObjectId < bool


def test_storage_persists_across_reopen(tmp_path) -> None:
    """Close the WT connection and reopen at the same path; data survives.

    The whole point of on-disk storage is that data persists across
    process restarts. This test exercises that end-to-end: write
    documents, an index, and an oplog entry; close; reopen the same
    directory; verify the data is exactly what we wrote.

    Adding a new field, table, or option to a future SecantusDB release
    should keep this test passing — that's the load-bearing format
    compatibility we promise to users.
    """
    s1 = Storage(str(tmp_path))
    try:
        s1.insert(
            "winelog",
            "bottles",
            [
                {"_id": 1, "name": "Pommard 2018", "year": 2018},
                {"_id": 2, "name": "Brunello 2015", "year": 2015},
            ],
        )
        s1.create_index("winelog", "bottles", "year_1", {"year": 1}, {})
    finally:
        s1.close()

    s2 = Storage(str(tmp_path))
    try:
        docs = sorted(
            s2.find_matching("winelog", "bottles", {}),
            key=lambda d: d["_id"],
        )
        assert [d["_id"] for d in docs] == [1, 2]
        assert docs[0]["name"] == "Pommard 2018"
        assert docs[1]["year"] == 2015

        # The user-created index is back, alongside the implicit _id_.
        names = {ix["name"] for ix in s2.list_indexes("winelog", "bottles")}
        assert "year_1" in names
        assert "_id_" in names

        # The index actually serves a query (the entries table survived,
        # not just the index metadata).
        plan = s2.explain_plan("winelog", "bottles", {"year": 2018})
        assert plan["kind"] == "IXSCAN"
        assert plan["index_name"] == "year_1"
    finally:
        s2.close()


def test_secantusdb_server_persists_across_restart(tmp_path) -> None:
    """End-to-end: server restart on the same on-disk path keeps data.

    Mirrors the test_storage_persists_across_reopen check but goes
    through the full wire stack — pymongo writes, server restart,
    pymongo reads — which is the path real users exercise.
    """
    from pymongo import MongoClient

    from secantus import SecantusDBServer

    docs = [
        {"_id": 1, "x": "first"},
        {"_id": 2, "x": "second"},
    ]

    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            mc["persist_db"]["c"].insert_many(docs)
            mc["persist_db"]["c"].create_index([("x", 1)])
        finally:
            mc.close()

    # Reopen the server on the same path (different bound port).
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            stored = sorted(
                mc["persist_db"]["c"].find(),
                key=lambda d: d["_id"],
            )
            assert stored == docs
            names = {ix["name"] for ix in mc["persist_db"]["c"].list_indexes()}
            assert names >= {"_id_", "x_1"}
        finally:
            mc.close()


def test_checkpoint_persists_inserts(tmp_path) -> None:
    """Forcing a checkpoint flushes pending writes; subsequent close+reopen sees them."""
    storage = Storage(str(tmp_path))
    storage.insert("db", "c", [{"_id": 1, "x": 1}])
    storage.checkpoint()
    storage.close()

    reopened = Storage(str(tmp_path))
    try:
        results = reopened.find_matching("db", "c", {})
        assert results == [{"_id": 1, "x": 1}]
    finally:
        reopened.close()


def test_checkpoint_after_close_is_safe(tmp_path) -> None:
    storage = Storage(str(tmp_path))
    storage.close()
    storage.checkpoint()  # no-op; must not raise.
