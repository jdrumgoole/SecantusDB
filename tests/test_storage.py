from __future__ import annotations

import bson
import pytest

from fongodb.storage import Storage


@pytest.fixture
def storage() -> Storage:
    return Storage(":memory:")


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
