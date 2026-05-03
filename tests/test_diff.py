from __future__ import annotations

from secantus.diff import compute_update_description


def test_diff_set_unset_inc() -> None:
    diff = compute_update_description({"_id": 1, "a": 1, "b": 2}, {"_id": 1, "a": 5, "c": 3})
    assert diff["updatedFields"] == {"a": 5, "c": 3}
    assert diff["removedFields"] == ["b"]
    assert diff["truncatedArrays"] == []


def test_diff_unchanged_yields_empty() -> None:
    diff = compute_update_description({"_id": 1, "x": 1}, {"_id": 1, "x": 1})
    assert diff == {"updatedFields": {}, "removedFields": [], "truncatedArrays": []}


def test_diff_nested_dotted_paths() -> None:
    diff = compute_update_description(
        {"_id": 1, "p": {"q": {"r": 1, "s": 2}}},
        {"_id": 1, "p": {"q": {"r": 9}}},
    )
    assert diff["updatedFields"] == {"p.q.r": 9}
    assert diff["removedFields"] == ["p.q.s"]
    assert diff["truncatedArrays"] == []


def test_diff_nested_added_field() -> None:
    diff = compute_update_description({"_id": 1, "p": {}}, {"_id": 1, "p": {"q": {"r": 7}}})
    # Whole subtree added — emit at the leaf ancestor level mongod-style.
    assert "p.q" in diff["updatedFields"]
    assert diff["updatedFields"]["p.q"] == {"r": 7}


def test_diff_array_truncation() -> None:
    diff = compute_update_description({"_id": 1, "a": [1, 2, 3, 4]}, {"_id": 1, "a": [1, 2]})
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 2}]
    assert diff["updatedFields"] == {}
    assert diff["removedFields"] == []


def test_diff_array_not_truncation_replaces_whole() -> None:
    diff = compute_update_description({"_id": 1, "a": [1, 2, 3]}, {"_id": 1, "a": [2, 3]})
    assert diff["updatedFields"] == {"a": [2, 3]}
    assert diff["truncatedArrays"] == []


def test_diff_array_grew_replaces_whole() -> None:
    diff = compute_update_description({"_id": 1, "a": [1]}, {"_id": 1, "a": [1, 2, 3]})
    assert diff["updatedFields"] == {"a": [1, 2, 3]}
    assert diff["truncatedArrays"] == []


def test_diff_replacement_doc() -> None:
    diff = compute_update_description({"_id": 1, "x": 1}, {"_id": 1, "y": 2})
    assert diff["updatedFields"] == {"y": 2}
    assert diff["removedFields"] == ["x"]
