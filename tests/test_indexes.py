from __future__ import annotations

import pytest

from secantus.storage import IndexConflict, Storage


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


def _index_entry_count(storage: Storage, db: str, coll: str, name: str | None = None) -> int:
    from secantus.storage import _IDX_ENTRIES_TABLE

    prefix: tuple = (db, coll) if name is None else (db, coll, name)
    return len(storage._collect_prefix(_IDX_ENTRIES_TABLE, prefix))


def test_index_lookup_uses_index_for_equality(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i % 5} for i in range(20)])
    docs = storage.find_matching("db", "c", {"x": 3})
    ids = sorted(d["_id"] for d in docs)
    assert ids == [3, 8, 13, 18]


def test_index_lookup_avoids_full_scan(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i % 5} for i in range(20)])
    calls: list[tuple[str, str]] = []
    real_scan = storage._scan_docs

    def spy(db: str, coll: str):
        calls.append((db, coll))
        return real_scan(db, coll)

    monkeypatch.setattr(storage, "_scan_docs", spy)
    docs = storage.find_matching("db", "c", {"x": 2})
    assert len(docs) == 4
    assert calls == [], "find_matching should not full-scan when an index covers the filter"


def test_index_lookup_falls_back_when_no_index(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": 1, "x": 1}, {"_id": 2, "x": 2}])
    docs = storage.find_matching("db", "c", {"x": 1})
    assert [d["_id"] for d in docs] == [1]


def test_index_entries_track_updates(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 10}, {"_id": 2, "x": 20}])
    storage.update_matching("db", "c", {"_id": 1}, {"$set": {"x": 99}})
    assert [d["_id"] for d in storage.find_matching("db", "c", {"x": 10})] == []
    assert [d["_id"] for d in storage.find_matching("db", "c", {"x": 99})] == [1]


def test_index_entries_removed_on_delete(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 10}, {"_id": 2, "x": 10}])
    assert _index_entry_count(storage, "db", "c", "x_1") == 2
    storage.delete_matching("db", "c", {"_id": 1})
    assert _index_entry_count(storage, "db", "c", "x_1") == 1
    assert [d["_id"] for d in storage.find_matching("db", "c", {"x": 10})] == [2]


def test_drop_index_removes_entries(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 10}, {"_id": 2, "x": 20}])
    assert _index_entry_count(storage, "db", "c", "x_1") == 2
    storage.drop_index("db", "c", "x_1")
    assert _index_entry_count(storage, "db", "c", "x_1") == 0


def test_drop_collection_removes_index_entries(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 10}, {"_id": 2, "x": 20}])
    storage.drop_collection("db", "c")
    assert _index_entry_count(storage, "db", "c") == 0


def test_drop_database_removes_index_entries(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 10}])
    storage.drop_database("db")
    assert _index_entry_count(storage, "db", "c") == 0


def test_drop_all_indexes_clears_entries(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.create_index("db", "c", "y_1", {"y": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 1, "y": 2}])
    assert _index_entry_count(storage, "db", "c") == 2
    storage.drop_all_indexes("db", "c")
    assert _index_entry_count(storage, "db", "c") == 0


def test_rename_collection_moves_index_entries(storage: Storage) -> None:
    storage.create_index("db", "src", "x_1", {"x": 1}, {})
    storage.insert("db", "src", [{"_id": 1, "x": 10}, {"_id": 2, "x": 20}])
    assert _index_entry_count(storage, "db", "src", "x_1") == 2
    storage.rename_collection("db", "src", "db", "dst")
    assert _index_entry_count(storage, "db", "src", "x_1") == 0
    assert _index_entry_count(storage, "db", "dst", "x_1") == 2
    docs = storage.find_matching("db", "dst", {"x": 10})
    assert [d["_id"] for d in docs] == [1]


def test_create_index_after_inserts_populates_entries(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": i, "x": i % 3} for i in range(9)])
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    assert _index_entry_count(storage, "db", "c", "x_1") == 9
    docs = storage.find_matching("db", "c", {"x": 2})
    assert sorted(d["_id"] for d in docs) == [2, 5, 8]


def test_unique_index_uses_entry_probe(storage: Storage) -> None:
    storage.create_index("db", "c", "email_1", {"email": 1}, {"unique": True})
    storage.insert("db", "c", [{"_id": 1, "email": "a@x"}])
    _, errors = storage.insert("db", "c", [{"_id": 2, "email": "a@x"}])
    assert errors and errors[0]["code"] == 11000
    assert _index_entry_count(storage, "db", "c", "email_1") == 1


def _spy_scans(storage: Storage, monkeypatch) -> list[tuple[str, str]]:
    calls: list[tuple[str, str]] = []
    real_scan = storage._scan_docs

    def spy(db: str, coll: str):
        calls.append((db, coll))
        return real_scan(db, coll)

    monkeypatch.setattr(storage, "_scan_docs", spy)
    return calls


def test_index_range_gt_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$gt": 6}})
    assert sorted(d["_id"] for d in docs) == [7, 8, 9]
    assert calls == []


def test_index_range_gte_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$gte": 6}})
    assert sorted(d["_id"] for d in docs) == [6, 7, 8, 9]
    assert calls == []


def test_index_range_lt_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$lt": 3}})
    assert sorted(d["_id"] for d in docs) == [0, 1, 2]
    assert calls == []


def test_index_range_lte_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$lte": 3}})
    assert sorted(d["_id"] for d in docs) == [0, 1, 2, 3]
    assert calls == []


def test_index_range_compound_bounds(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$gte": 5, "$lt": 10}})
    assert sorted(d["_id"] for d in docs) == [5, 6, 7, 8, 9]
    assert calls == []


def test_index_in_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$in": [2, 5, 9]}})
    assert sorted(d["_id"] for d in docs) == [2, 5, 9]
    assert calls == []


def test_index_eq_operator_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i % 3} for i in range(9)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$eq": 1}})
    assert sorted(d["_id"] for d in docs) == [1, 4, 7]
    assert calls == []


def test_index_range_with_unsupported_op_falls_back(storage: Storage, monkeypatch) -> None:
    """A range op mixed with an op the planner doesn't handle falls back."""
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$gt": 3, "$ne": 5}})
    assert sorted(d["_id"] for d in docs) == [4, 6, 7, 8, 9]
    assert calls != []


def test_index_range_string(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "name_1", {"name": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "name": "alice"},
            {"_id": 2, "name": "bob"},
            {"_id": 3, "name": "carol"},
            {"_id": 4, "name": "dave"},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"name": {"$gte": "b", "$lt": "d"}})
    assert sorted(d["_id"] for d in docs) == [2, 3]
    assert calls == []


def test_index_range_mixed_int_and_decimal(storage: Storage) -> None:
    from bson import Decimal128

    storage.create_index("db", "c", "n_1", {"n": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "n": 1},
            {"_id": 2, "n": 2.5},
            {"_id": 3, "n": Decimal128("3.5")},
            {"_id": 4, "n": 5},
        ],
    )
    docs = storage.find_matching("db", "c", {"n": {"$gte": 2, "$lte": 4}})
    assert sorted(d["_id"] for d in docs) == [2, 3]


def test_index_in_with_empty_list(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 1}, {"_id": 2, "x": 2}])
    docs = storage.find_matching("db", "c", {"x": {"$in": []}})
    assert docs == []
