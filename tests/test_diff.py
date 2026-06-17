from __future__ import annotations

import copy
import random

import pytest

from secantus.diff import apply_update_description, compute_update_description


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


def test_disambiguated_paths_for_numeric_dict_keys() -> None:
    """mongod 6.1+: updated paths containing numeric-string FIELD names
    get a disambiguatedPaths entry mapping the dotted path to typed
    segments (int = array index, str = field name)."""
    out = compute_update_description({"a": {"1": 1}}, {"a": {"1": 2}})
    assert out["updatedFields"] == {"a.1": 2}
    assert out["disambiguatedPaths"] == {"a.1": ["a", "1"]}

    out = compute_update_description({"a": [{"1": 1}]}, {"a": [{"1": 2}]})
    assert out["updatedFields"] == {"a.0.1": 2}
    assert out["disambiguatedPaths"] == {"a.0.1": ["a", 0, "1"]}


def test_disambiguated_paths_absent_when_unambiguous() -> None:
    out = compute_update_description({"a": {"b": 1}}, {"a": {"b": 2}})
    assert "disambiguatedPaths" not in out

    # Plain array index paths are NOT ambiguous on their own.
    out = compute_update_description({"a": [1, 2]}, {"a": [9, 2]})
    assert out["updatedFields"] == {"a.0": 9}
    assert "disambiguatedPaths" not in out


def test_disambiguated_paths_for_removed_and_truncated() -> None:
    out = compute_update_description({"a": {"1": 1}, "b": 0}, {"b": 0})
    assert out["removedFields"] == ["a"]
    assert "disambiguatedPaths" not in out  # "a" itself is unambiguous

    out = compute_update_description({"a": {"2": [1, 2, 3]}}, {"a": {"2": [1]}})
    assert out["truncatedArrays"] == [{"field": "a.2", "newSize": 1}]
    assert out["disambiguatedPaths"] == {"a.2": ["a", "2"]}


# --- apply_update_description: inverse round-trip (PITR oplog replay) -------


def test_apply_set_unset_truncate() -> None:
    pre = {"_id": 1, "a": 1, "b": 2, "d": [1, 2, 3]}
    diff = {
        "updatedFields": {"a": 9, "c": 3},
        "removedFields": ["b"],
        "truncatedArrays": [{"field": "d", "newSize": 2}],
    }
    assert apply_update_description(pre, diff) == {"_id": 1, "a": 9, "c": 3, "d": [1, 2]}


def test_apply_empty_diff_is_noop() -> None:
    doc = {"_id": 1, "x": [1, 2], "y": {"z": 1}}
    assert apply_update_description(copy.deepcopy(doc), {}) == doc


def _rand_value(rng: random.Random, depth: int) -> object:
    kinds = ["int", "str", "bool", "none"]
    if depth < 3:
        kinds += ["dict", "list"]
    kind = rng.choice(kinds)
    if kind == "int":
        return rng.randint(-50, 50)
    if kind == "str":
        return rng.choice(["a", "bb", "ccc", ""])
    if kind == "bool":
        return rng.choice([True, False])
    if kind == "none":
        return None
    if kind == "dict":
        # Include numeric-string keys to exercise the disambiguation path.
        return {
            rng.choice(["x", "y", "0", "1", "n"]): _rand_value(rng, depth + 1)
            for _ in range(rng.randint(0, 3))
        }
    return [_rand_value(rng, depth + 1) for _ in range(rng.randint(0, 4))]


def _rand_doc(rng: random.Random) -> dict[str, object]:
    doc: dict[str, object] = {"_id": 1}
    for _ in range(rng.randint(0, 5)):
        doc[rng.choice(["a", "b", "c", "0", "1", "nested"])] = _rand_value(rng, 0)
    return doc


@pytest.mark.parametrize("seed", range(300))
def test_apply_is_inverse_of_compute(seed: int) -> None:
    """For any pre/post, applying the computed diff to the pre-image must
    reconstruct the post-image exactly. This is the oracle the Rust port's
    parity suite checks against."""
    rng = random.Random(seed)
    pre = _rand_doc(rng)
    post = _rand_doc(rng)
    diff = compute_update_description(pre, post)
    rebuilt = apply_update_description(copy.deepcopy(pre), diff)
    assert rebuilt == post, f"seed={seed}\npre={pre}\npost={post}\ndiff={diff}\ngot={rebuilt}"
