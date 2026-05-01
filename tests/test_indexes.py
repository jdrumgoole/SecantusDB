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


def test_sort_indexed_field_no_filter_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": (i * 7) % 11} for i in range(11)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {}, sort={"x": 1})
    xs = [d["x"] for d in docs]
    assert xs == sorted(xs)
    assert calls == []


def test_sort_indexed_field_descending_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(8)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {}, sort={"x": -1})
    assert [d["x"] for d in docs] == [7, 6, 5, 4, 3, 2, 1, 0]
    assert calls == []


def test_sort_indexed_field_with_range_filter_no_resort(storage: Storage, monkeypatch) -> None:
    import secantus.storage as st

    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    sort_docs_call_count = [0]
    real = st.sort_docs

    def counting_sort(*a, **kw):
        sort_docs_call_count[0] += 1
        return real(*a, **kw)

    monkeypatch.setattr(st, "sort_docs", counting_sort)
    docs = storage.find_matching("db", "c", {"x": {"$gte": 5, "$lt": 10}}, sort={"x": 1})
    assert [d["_id"] for d in docs] == [5, 6, 7, 8, 9]
    assert calls == []
    assert sort_docs_call_count[0] == 0


def test_sort_indexed_field_descending_with_filter_reverses(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$gte": 5, "$lt": 10}}, sort={"x": -1})
    assert [d["_id"] for d in docs] == [9, 8, 7, 6, 5]
    assert calls == []


def test_sort_on_unindexed_field_falls_back(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i, "y": -i} for i in range(5)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {}, sort={"y": 1})
    assert [d["_id"] for d in docs] == [4, 3, 2, 1, 0]
    assert calls != []


def test_sort_index_walk_does_not_leak_other_indexes(storage: Storage) -> None:
    """Walking one index shouldn't include entries from another index of the same coll."""
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.create_index("db", "c", "y_1", {"y": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i, "y": 100 - i} for i in range(5)])
    docs = storage.find_matching("db", "c", {}, sort={"x": 1})
    assert [d["_id"] for d in docs] == [0, 1, 2, 3, 4]
    docs = storage.find_matching("db", "c", {}, sort={"y": 1})
    assert [d["_id"] for d in docs] == [4, 3, 2, 1, 0]


def test_compound_index_full_match_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
            {"_id": 3, "a": 2, "b": 10},
            {"_id": 4, "a": 2, "b": 20},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": 20})
    assert [d["_id"] for d in docs] == [2]
    assert calls == []


def test_compound_index_leading_prefix_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
            {"_id": 3, "a": 2, "b": 10},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1})
    assert sorted(d["_id"] for d in docs) == [1, 2]
    assert calls == []


def test_compound_index_three_field_prefix(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "abc_1", {"a": 1, "b": 1, "c": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10, "c": "x"},
            {"_id": 2, "a": 1, "b": 10, "c": "y"},
            {"_id": 3, "a": 1, "b": 20, "c": "x"},
            {"_id": 4, "a": 2, "b": 10, "c": "x"},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": 10})
    assert sorted(d["_id"] for d in docs) == [1, 2]
    assert calls == []


def test_compound_index_filter_field_order_does_not_matter(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"b": 20, "a": 1})  # filter order: b first
    assert [d["_id"] for d in docs] == [2]
    assert calls == []


def test_compound_index_skipping_leading_field_falls_back(storage: Storage, monkeypatch) -> None:
    """Filter on b alone (not the leading field) cannot use the {a,b} index."""
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert("db", "c", [{"_id": i, "a": 1, "b": i} for i in range(5)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"b": 3})
    assert [d["_id"] for d in docs] == [3]
    assert calls != []  # full scan


def test_compound_index_filter_with_extra_fields_falls_back(storage: Storage, monkeypatch) -> None:
    """Filter has fields beyond what the index covers — needs post-filter, defer."""
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10, "z": "x"},
            {"_id": 2, "a": 1, "b": 10, "z": "y"},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": 10, "z": "x"})
    assert [d["_id"] for d in docs] == [1]
    # Index covers only {a, b}; the z filter forced a full scan in this MVP.
    assert calls != []


def test_single_field_filter_uses_compound_index_when_no_single_index(
    storage: Storage, monkeypatch
) -> None:
    """Single-field bare-equality on the leading field of a compound index uses it."""
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 2, "b": 20},
            {"_id": 3, "a": 1, "b": 30},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1})
    assert sorted(d["_id"] for d in docs) == [1, 3]
    assert calls == []


def test_compound_index_prefers_exact_cover_over_longer_index(
    storage: Storage, monkeypatch
) -> None:
    """Two indexes match the prefix; pick the tighter one."""
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.create_index("db", "c", "abc_1", {"a": 1, "b": 1, "c": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10, "c": "x"},
            {"_id": 2, "a": 1, "b": 10, "c": "y"},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": 10})
    assert sorted(d["_id"] for d in docs) == [1, 2]
    assert calls == []


def test_compound_index_unique_violation_detected(storage: Storage) -> None:
    """Unique compound enforcement still works through the new path."""
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {"unique": True})
    storage.insert("db", "c", [{"_id": 1, "a": 1, "b": 10}])
    _, errors = storage.insert("db", "c", [{"_id": 2, "a": 1, "b": 10}])
    assert errors and errors[0]["code"] == 11000


def test_compound_index_update_maintains_entries(storage: Storage) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "a": 1, "b": 10}])
    storage.update_matching("db", "c", {"_id": 1}, {"$set": {"b": 99}})
    assert storage.find_matching("db", "c", {"a": 1, "b": 10}) == []
    docs = storage.find_matching("db", "c", {"a": 1, "b": 99})
    assert [d["_id"] for d in docs] == [1]


def test_compound_index_eq_plus_gt_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert("db", "c", [{"_id": i, "a": (i // 5) + 1, "b": i % 5} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 2, "b": {"$gt": 2}})
    assert sorted(d["_id"] for d in docs) == [8, 9]
    assert calls == []


def test_compound_index_eq_plus_gte_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert("db", "c", [{"_id": i, "a": (i // 5) + 1, "b": i % 5} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 2, "b": {"$gte": 3}})
    assert sorted(d["_id"] for d in docs) == [8, 9]
    assert calls == []


def test_compound_index_eq_plus_lt_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert("db", "c", [{"_id": i, "a": (i // 5) + 1, "b": i % 5} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 2, "b": {"$lt": 2}})
    assert sorted(d["_id"] for d in docs) == [5, 6]
    assert calls == []


def test_compound_index_eq_plus_range_both_bounds(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [{"_id": i, "a": 1, "b": i} for i in range(20)]
        + [{"_id": 100 + i, "a": 2, "b": i} for i in range(5)],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$gte": 5, "$lt": 10}})
    assert sorted(d["_id"] for d in docs) == [5, 6, 7, 8, 9]
    assert calls == []


def test_compound_index_eq_plus_in_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
            {"_id": 3, "a": 1, "b": 30},
            {"_id": 4, "a": 2, "b": 10},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$in": [10, 30]}})
    assert sorted(d["_id"] for d in docs) == [1, 3]
    assert calls == []


def test_compound_index_eq_plus_eq_operator(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$eq": 20}})
    assert [d["_id"] for d in docs] == [2]
    assert calls == []


def test_compound_three_field_eq_plus_range_on_third(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "abc_1", {"a": 1, "b": 1, "c": 1}, {})
    storage.insert(
        "db",
        "c",
        [{"_id": i, "a": 1, "b": 10, "c": i} for i in range(10)]
        + [{"_id": 100 + i, "a": 1, "b": 20, "c": i} for i in range(5)]
        + [{"_id": 200 + i, "a": 2, "b": 10, "c": i} for i in range(5)],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": 10, "c": {"$gt": 5}})
    assert sorted(d["_id"] for d in docs) == [6, 7, 8, 9]
    assert calls == []


def test_compound_range_does_not_leak_across_eq_prefix(storage: Storage, monkeypatch) -> None:
    """A range that would match docs in another (a) bucket must stop at the prefix boundary."""
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 100},
            {"_id": 2, "a": 1, "b": 200},
            {"_id": 3, "a": 2, "b": 1},
            {"_id": 4, "a": 2, "b": 5},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$gte": 50}})
    assert sorted(d["_id"] for d in docs) == [1, 2]
    assert calls == []


def test_compound_index_range_skipping_middle_field_falls_back(
    storage: Storage, monkeypatch
) -> None:
    """Index {a,b,c}; filter {a:1, c:{$gt:5}} skips b — planner can't use the index."""
    storage.create_index("db", "c", "abc_1", {"a": 1, "b": 1, "c": 1}, {})
    storage.insert("db", "c", [{"_id": i, "a": 1, "b": i, "c": i * 2} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "c": {"$gt": 10}})
    assert sorted(d["_id"] for d in docs) == [6, 7, 8, 9]
    assert calls != []


def test_compound_index_range_in_with_empty_list(storage: Storage) -> None:
    storage.create_index("db", "c", "ab_1", {"a": 1, "b": 1}, {})
    storage.insert("db", "c", [{"_id": 1, "a": 1, "b": 10}])
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$in": []}})
    assert docs == []


def test_hint_by_name_walks_named_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(5)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": 3}, hint="x_1")
    assert [d["_id"] for d in docs] == [3]
    assert calls == []


def test_hint_by_key_spec_walks_matching_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(5)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {}, hint={"x": 1})
    assert sorted(d["_id"] for d in docs) == [0, 1, 2, 3, 4]
    assert calls == []


def test_hint_unknown_index_raises(storage: Storage) -> None:
    from secantus.storage import BadHint

    storage.insert("db", "c", [{"x": 1}])
    with pytest.raises(BadHint):
        storage.find_matching("db", "c", {}, hint="nonexistent")
    with pytest.raises(BadHint):
        storage.find_matching("db", "c", {}, hint={"nonexistent": 1})


def test_hint_natural_uses_collection_scan(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(3)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": 1}, hint="$natural")
    assert [d["_id"] for d in docs] == [1]
    assert calls != []


def test_hint_natural_dict_form(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(3)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": 1}, hint={"$natural": 1})
    assert [d["_id"] for d in docs] == [1]
    assert calls != []


def test_hint_id_index(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": i, "x": -i} for i in range(5)])
    docs = storage.find_matching("db", "c", {}, hint="_id_")
    assert sorted(d["_id"] for d in docs) == [0, 1, 2, 3, 4]
    docs = storage.find_matching("db", "c", {}, hint={"_id": 1})
    assert sorted(d["_id"] for d in docs) == [0, 1, 2, 3, 4]


def test_hint_with_filter_post_filters(storage: Storage, monkeypatch) -> None:
    """hint forces using the named index even when the filter doesn't fit."""
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i, "y": i % 2} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"y": 1}, hint="x_1")
    assert sorted(d["_id"] for d in docs) == [1, 3, 5, 7, 9]
    assert calls == []


def test_hint_with_sort_matching_index_skips_sort(storage: Storage, monkeypatch) -> None:
    import secantus.storage as st

    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": (i * 7) % 11} for i in range(11)])
    sort_calls = [0]
    real = st.sort_docs

    def counting(*a, **kw):
        sort_calls[0] += 1
        return real(*a, **kw)

    monkeypatch.setattr(st, "sort_docs", counting)
    docs = storage.find_matching("db", "c", {}, sort={"x": 1}, hint="x_1")
    xs = [d["x"] for d in docs]
    assert xs == sorted(xs)
    assert sort_calls[0] == 0


def test_hint_with_descending_sort_reverses(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(5)])
    docs = storage.find_matching("db", "c", {}, sort={"x": -1}, hint="x_1")
    assert [d["x"] for d in docs] == [4, 3, 2, 1, 0]


def test_hint_with_sort_on_different_field_still_sorts(storage: Storage) -> None:
    storage.create_index("db", "c", "x_1", {"x": 1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i, "y": -i} for i in range(5)])
    docs = storage.find_matching("db", "c", {}, sort={"y": 1}, hint="x_1")
    assert [d["_id"] for d in docs] == [4, 3, 2, 1, 0]


def test_desc_index_equality_lookup(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i % 5} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": 3})
    assert sorted(d["_id"] for d in docs) == [3, 8, 13, 18]
    assert calls == []


def test_desc_index_in_lookup(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$in": [2, 5, 9]}})
    assert sorted(d["_id"] for d in docs) == [2, 5, 9]
    assert calls == []


def test_desc_index_gt_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$gt": 6}})
    assert sorted(d["_id"] for d in docs) == [7, 8, 9]
    assert calls == []


def test_desc_index_lt_uses_index(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$lt": 3}})
    assert sorted(d["_id"] for d in docs) == [0, 1, 2]
    assert calls == []


def test_desc_index_range_both_bounds(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"x": {"$gte": 5, "$lt": 10}})
    assert sorted(d["_id"] for d in docs) == [5, 6, 7, 8, 9]
    assert calls == []


def test_desc_index_sort_descending_walks_forward(storage: Storage, monkeypatch) -> None:
    """Sort {x:-1} on a {x:-1} index should walk forward (no list-reversal)."""
    import secantus.storage as st

    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(8)])
    sort_calls = [0]
    real = st.sort_docs

    def counting(*a, **kw):
        sort_calls[0] += 1
        return real(*a, **kw)

    monkeypatch.setattr(st, "sort_docs", counting)
    docs = storage.find_matching("db", "c", {}, sort={"x": -1})
    assert [d["x"] for d in docs] == [7, 6, 5, 4, 3, 2, 1, 0]
    assert sort_calls[0] == 0


def test_desc_index_sort_ascending_reverses(storage: Storage) -> None:
    """Sort {x:1} on a {x:-1} index walks backward through the descending bytes."""
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(5)])
    docs = storage.find_matching("db", "c", {}, sort={"x": 1})
    assert [d["x"] for d in docs] == [0, 1, 2, 3, 4]


def test_desc_index_filter_plus_sort_no_resort(storage: Storage, monkeypatch) -> None:
    """Range filter + matching DESC sort: index walk already in order."""
    import secantus.storage as st

    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(20)])
    sort_calls = [0]
    real = st.sort_docs

    def counting(*a, **kw):
        sort_calls[0] += 1
        return real(*a, **kw)

    monkeypatch.setattr(st, "sort_docs", counting)
    docs = storage.find_matching("db", "c", {"x": {"$gte": 5, "$lt": 10}}, sort={"x": -1})
    assert [d["_id"] for d in docs] == [9, 8, 7, 6, 5]
    assert sort_calls[0] == 0


def test_desc_index_unique_enforcement(storage: Storage) -> None:
    storage.create_index("db", "c", "email_-1", {"email": -1}, {"unique": True})
    storage.insert("db", "c", [{"_id": 1, "email": "a@x"}])
    _, errors = storage.insert("db", "c", [{"_id": 2, "email": "a@x"}])
    assert errors and errors[0]["code"] == 11000


def test_desc_index_update_maintains_entries(storage: Storage) -> None:
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": 1, "x": 10}])
    storage.update_matching("db", "c", {"_id": 1}, {"$set": {"x": 99}})
    assert storage.find_matching("db", "c", {"x": 10}) == []
    assert [d["_id"] for d in storage.find_matching("db", "c", {"x": 99})] == [1]


def test_desc_index_via_hint(storage: Storage) -> None:
    storage.create_index("db", "c", "x_-1", {"x": -1}, {})
    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(5)])
    docs = storage.find_matching("db", "c", {}, hint="x_-1", sort={"x": -1})
    assert [d["x"] for d in docs] == [4, 3, 2, 1, 0]
    docs = storage.find_matching("db", "c", {}, hint={"x": -1}, sort={"x": -1})
    assert [d["x"] for d in docs] == [4, 3, 2, 1, 0]


def test_mixed_compound_eq_full_match(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
            {"_id": 3, "a": 2, "b": 10},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": 10})
    assert [d["_id"] for d in docs] == [1]
    assert calls == []


def test_mixed_compound_prefix_lookup(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
            {"_id": 3, "a": 2, "b": 30},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1})
    assert sorted(d["_id"] for d in docs) == [1, 2]
    assert calls == []


def test_mixed_compound_eq_plus_gt_on_desc_trailing(storage: Storage, monkeypatch) -> None:
    """Filter {a:1, b:{$gt:15}} on {a:1, b:-1} index: bounds flip for the DESC b."""
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {})
    storage.insert("db", "c", [{"_id": i, "a": 1, "b": i} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$gt": 15}})
    assert sorted(d["_id"] for d in docs) == [16, 17, 18, 19]
    assert calls == []


def test_mixed_compound_eq_plus_lt_on_desc_trailing(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {})
    storage.insert("db", "c", [{"_id": i, "a": 1, "b": i} for i in range(10)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$lt": 3}})
    assert sorted(d["_id"] for d in docs) == [0, 1, 2]
    assert calls == []


def test_mixed_compound_eq_plus_in(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {})
    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "a": 1, "b": 10},
            {"_id": 2, "a": 1, "b": 20},
            {"_id": 3, "a": 1, "b": 30},
            {"_id": 4, "a": 2, "b": 10},
        ],
    )
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$in": [10, 30]}})
    assert sorted(d["_id"] for d in docs) == [1, 3]
    assert calls == []


def test_mixed_compound_eq_plus_range_both_bounds(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {})
    storage.insert("db", "c", [{"_id": i, "a": 1, "b": i} for i in range(20)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": {"$gte": 5, "$lt": 10}})
    assert sorted(d["_id"] for d in docs) == [5, 6, 7, 8, 9]
    assert calls == []


def test_mixed_compound_unique_enforcement(storage: Storage) -> None:
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {"unique": True})
    storage.insert("db", "c", [{"_id": 1, "a": 1, "b": 10}])
    _, errors = storage.insert("db", "c", [{"_id": 2, "a": 1, "b": 10}])
    assert errors and errors[0]["code"] == 11000


def test_mixed_compound_update_maintains_entries(storage: Storage) -> None:
    storage.create_index("db", "c", "ab_mixed", {"a": 1, "b": -1}, {})
    storage.insert("db", "c", [{"_id": 1, "a": 1, "b": 10}])
    storage.update_matching("db", "c", {"_id": 1}, {"$set": {"b": 99}})
    assert storage.find_matching("db", "c", {"a": 1, "b": 10}) == []
    assert [d["_id"] for d in storage.find_matching("db", "c", {"a": 1, "b": 99})] == [1]


def test_pure_desc_compound_eq(storage: Storage, monkeypatch) -> None:
    storage.create_index("db", "c", "ab_-1", {"a": -1, "b": -1}, {})
    storage.insert("db", "c", [{"_id": i, "a": i % 3, "b": i} for i in range(9)])
    calls = _spy_scans(storage, monkeypatch)
    docs = storage.find_matching("db", "c", {"a": 1, "b": 4})
    assert [d["_id"] for d in docs] == [4]
    assert calls == []
