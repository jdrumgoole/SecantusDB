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
    """Strict head-prefix shrink: just emit truncatedArrays."""
    diff = compute_update_description({"_id": 1, "a": [1, 2, 3, 4]}, {"_id": 1, "a": [1, 2]})
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 2}]
    assert diff["updatedFields"] == {}
    assert diff["removedFields"] == []


def test_diff_array_shift_left_indexed_updates() -> None:
    """pre=[1,2,3], post=[2,3]: shrink with both kept positions changed.

    mongod's $v:2 represents this as indexed updates at positions 0 and
    1 plus a truncatedArrays size change — NOT a wholesale replace.
    """
    diff = compute_update_description({"_id": 1, "a": [1, 2, 3]}, {"_id": 1, "a": [2, 3]})
    assert diff["updatedFields"] == {"a.0": 2, "a.1": 3}
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 2}]
    assert diff["removedFields"] == []


def test_diff_array_truncation_with_changed_tail_element() -> None:
    """pre=[1,2,3,4], post=[1,2,99]: kept-prefix change at position 2 + shrink."""
    diff = compute_update_description({"_id": 1, "a": [1, 2, 3, 4]}, {"_id": 1, "a": [1, 2, 99]})
    assert diff["updatedFields"] == {"a.2": 99}
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 3}]


def test_diff_array_truncation_with_subdoc_element_change() -> None:
    """Sub-doc element diff lands as arr.<i>.<field>, not the whole element."""
    diff = compute_update_description(
        {"_id": 1, "a": [{"x": 1}, {"x": 2}, {"x": 3}]},
        {"_id": 1, "a": [{"x": 1}, {"x": 99}]},
    )
    assert diff["updatedFields"] == {"a.1.x": 99}
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 2}]


def test_diff_array_same_length_indexed_updates() -> None:
    """Same-length array with a single element change: indexed update only."""
    diff = compute_update_description({"_id": 1, "a": [1, 2, 3]}, {"_id": 1, "a": [1, 99, 3]})
    assert diff["updatedFields"] == {"a.1": 99}
    assert diff["truncatedArrays"] == []


def test_diff_array_same_length_subdoc_indexed_update() -> None:
    """Same-length array of sub-docs: changed leaf emits ``arr.<i>.<field>``."""
    diff = compute_update_description(
        {"_id": 1, "a": [{"x": 1}, {"x": 2}]},
        {"_id": 1, "a": [{"x": 1}, {"x": 99}]},
    )
    assert diff["updatedFields"] == {"a.1.x": 99}
    assert diff["truncatedArrays"] == []


def test_diff_array_emptied() -> None:
    """Truncating an array to length 0 emits truncatedArrays with newSize=0."""
    diff = compute_update_description({"_id": 1, "a": [1, 2]}, {"_id": 1, "a": []})
    assert diff["updatedFields"] == {}
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 0}]


def test_diff_nested_array_truncation() -> None:
    """Truncation inside a sub-doc: path resolves to ``parent.array``."""
    diff = compute_update_description(
        {"_id": 1, "p": {"q": [1, 2, 3]}},
        {"_id": 1, "p": {"q": [1, 2]}},
    )
    assert diff["truncatedArrays"] == [{"field": "p.q", "newSize": 2}]
    assert diff["updatedFields"] == {}


def test_diff_array_grew_replaces_whole() -> None:
    """Growth (post longer than pre) still wholesale-replaces — mongod's
    $v:2 can encode appends but our simpler model treats "grew" as
    wholesale so downstream consumers re-fetch."""
    diff = compute_update_description({"_id": 1, "a": [1]}, {"_id": 1, "a": [1, 2, 3]})
    assert diff["updatedFields"] == {"a": [1, 2, 3]}
    assert diff["truncatedArrays"] == []


def test_diff_replacement_doc() -> None:
    diff = compute_update_description({"_id": 1, "x": 1}, {"_id": 1, "y": 2})
    assert diff["updatedFields"] == {"y": 2}
    assert diff["removedFields"] == ["x"]
