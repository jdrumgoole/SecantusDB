from __future__ import annotations

import pytest

from fongodb.storage import IndexConflict, Storage


@pytest.fixture
def storage() -> Storage:
    return Storage(":memory:")


def test_create_and_list_simple_index(storage: Storage) -> None:
    storage.insert("db", "c", [{"x": 1}])
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    indexes = storage.list_indexes("db", "c")
    names = sorted(i["name"] for i in indexes)
    assert names == ["_id_", "x_1"]


def test_unique_index_rejects_duplicate_at_creation(storage: Storage) -> None:
    storage.insert("db", "c", [{"email": "a@x"}, {"email": "a@x"}])
    with pytest.raises(IndexConflict):
        storage.create_index("db", "c", "email_1", {"email": 1}, {"unique": True})


def test_unique_index_blocks_duplicate_insert(storage: Storage) -> None:
    storage.create_index("db", "c", "email_1", {"email": 1}, {"unique": True})
    inserted, _ = storage.insert("db", "c", [{"email": "a@x"}])
    assert inserted == 1
    inserted2, errors = storage.insert("db", "c", [{"email": "a@x"}])
    assert inserted2 == 0
    assert errors and errors[0]["code"] == 11000


def test_unique_compound_index(storage: Storage) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {"unique": True})
    inserted, _ = storage.insert("db", "c", [{"a": 1, "b": 1}, {"a": 1, "b": 2}, {"a": 2, "b": 1}])
    assert inserted == 3
    _, errors = storage.insert("db", "c", [{"a": 1, "b": 2}])
    assert errors and errors[0]["code"] == 11000


def test_sparse_unique_allows_multiple_missing(storage: Storage) -> None:
    storage.create_index("db", "c", "email_1", {"email": 1}, {"unique": True, "sparse": True})
    inserted, errors = storage.insert(
        "db", "c", [{"_id": 1}, {"_id": 2}, {"_id": 3, "email": "x@y"}]
    )
    assert inserted == 3
    assert not errors


def test_non_sparse_unique_treats_missing_as_null(storage: Storage) -> None:
    storage.create_index("db", "c", "email_1", {"email": 1}, {"unique": True})
    inserted, _ = storage.insert("db", "c", [{"_id": 1}])
    assert inserted == 1
    _, errors = storage.insert("db", "c", [{"_id": 2}])
    assert errors and errors[0]["code"] == 11000


def test_update_blocks_when_it_would_violate_unique(storage: Storage) -> None:
    storage.create_index("db", "c", "email_1", {"email": 1}, {"unique": True})
    storage.insert("db", "c", [{"_id": 1, "email": "a@x"}, {"_id": 2, "email": "b@x"}])
    with pytest.raises(IndexConflict):
        storage.update_matching("db", "c", {"_id": 2}, {"$set": {"email": "a@x"}})


def test_update_to_same_value_is_fine(storage: Storage) -> None:
    storage.create_index("db", "c", "email_1", {"email": 1}, {"unique": True})
    storage.insert("db", "c", [{"_id": 1, "email": "a@x"}])
    result = storage.update_matching(
        "db", "c", {"_id": 1}, {"$set": {"email": "a@x", "updated": True}}
    )
    assert result["modified"] == 1


def test_drop_index_removes_it(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    assert storage.drop_index("db", "c", "x_1") is True
    assert storage.drop_index("db", "c", "x_1") is False
    names = [i["name"] for i in storage.list_indexes("db", "c")]
    assert names == ["_id_"]


def test_cannot_drop_id_index(storage: Storage) -> None:
    storage.insert("db", "c", [{"x": 1}])
    assert storage.drop_index("db", "c", "_id_") is False


def test_drop_all_indexes_keeps_id(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.create_index("db", "c", "y_1", {"y": 1}, {})
    storage.drop_all_indexes("db", "c")
    names = [i["name"] for i in storage.list_indexes("db", "c")]
    assert names == ["_id_"]


def test_numeric_bridge_in_unique_index(storage: Storage) -> None:
    storage.create_index("db", "c", "n_1", {"n": 1}, {"unique": True})
    inserted, _ = storage.insert("db", "c", [{"n": 1}])
    assert inserted == 1
    _, errors = storage.insert("db", "c", [{"n": 1.0}])
    assert errors and errors[0]["code"] == 11000


def test_drop_collection_drops_indexes(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.drop_collection("db", "c")
    assert storage.list_indexes("db", "c") == []
