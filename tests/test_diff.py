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


# --- Arrays: reported by the OPERATION, not by diffing the values -----------
#
# These tests used to assert a value-diff: a shrink produced `truncatedArrays`
# plus indexed updates for the kept prefix, and one docstring claimed that was
# "mongod's $v:2 ... NOT a wholesale replace". Measured against mongod 8.2.11
# (and 6.0.16, and 8.3.4) that is wrong in both halves. mongod sends the WHOLE
# array for `$pop` / `$pull` / `$pullAll` / sliced `$push` / whole-field `$set`,
# reports `arr.<i>` only for an append or an indexed write, and never emits
# `truncatedArrays` at all -- not even popping one element off a 1000-element
# array. See tools/probes/change_streams.py and the backlog table.
#
# Because mongod reports the operation rather than the values, the update spec
# is now an argument: `$set: {arr: [...]}` and `$push: {arr: {$each: [...]}}`
# can produce an identical document and are reported differently. Omitting it
# is safe and means wholesale, which is what mongod does for most operators.


def test_array_shrink_sends_the_whole_array() -> None:
    """`$pop` does not produce `truncatedArrays` -- mongod resends the array."""
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2, 3, 4]}, {"_id": 1, "a": [1, 2]}, {"$pop": {"a": 1}}
    )
    assert diff["updatedFields"] == {"a": [1, 2]}
    assert diff["truncatedArrays"] == []
    assert diff["removedFields"] == []


def test_array_shift_left_sends_the_whole_array() -> None:
    """pre=[1,2,3] post=[2,3] via `$pop: -1`: whole array, no indexed updates."""
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2, 3]}, {"_id": 1, "a": [2, 3]}, {"$pop": {"a": -1}}
    )
    assert diff["updatedFields"] == {"a": [2, 3]}
    assert diff["truncatedArrays"] == []


def test_array_emptied_sends_the_empty_array() -> None:
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2]}, {"_id": 1, "a": []}, {"$pull": {"a": {"$gte": 1}}}
    )
    assert diff["updatedFields"] == {"a": []}
    assert diff["truncatedArrays"] == []


def test_nested_array_shrink_sends_the_whole_array() -> None:
    diff = compute_update_description(
        {"_id": 1, "p": {"q": [1, 2, 3]}},
        {"_id": 1, "p": {"q": [1, 2]}},
        {"$pop": {"p.q": 1}},
    )
    assert diff["updatedFields"] == {"p.q": [1, 2]}
    assert diff["truncatedArrays"] == []


def test_pipeline_update_truncation_still_reports_truncated_arrays() -> None:
    """The ONE shape where mongod really does emit `truncatedArrays`: an
    aggregation-PIPELINE update. `[{$set: {a: [...shorter...]}}]` reports a
    truncation where the same `$set` as an operator resends the whole array.
    This is what pymongo's unified "Test array truncation" spec asserts."""
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2, 3, 4]},
        {"_id": 1, "a": [1, 2]},
        [{"$set": {"a": [1, 2]}}],
    )
    assert diff["updatedFields"] == {}
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 2}]


def test_pipeline_update_growth_reports_appended_indices() -> None:
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2]},
        {"_id": 1, "a": [1, 2, 9]},
        [{"$set": {"a": [1, 2, 9]}}],
    )
    assert diff["updatedFields"] == {"a.2": 9}
    assert diff["truncatedArrays"] == []


def test_no_update_spec_diffs_the_values() -> None:
    """Without an update the function cannot know the operation, so it diffs
    values -- the pipeline behaviour, not the operator one."""
    diff = compute_update_description({"_id": 1, "a": [1, 2, 3, 4]}, {"_id": 1, "a": [1, 2]})
    assert diff["truncatedArrays"] == [{"field": "a", "newSize": 2}]


def test_indexed_set_reports_only_that_index() -> None:
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2, 3]}, {"_id": 1, "a": [1, 99, 3]}, {"$set": {"a.1": 99}}
    )
    assert diff["updatedFields"] == {"a.1": 99}
    assert diff["truncatedArrays"] == []


def test_indexed_set_into_a_subdoc_element() -> None:
    """A changed leaf emits ``arr.<i>.<field>``, not the whole element."""
    diff = compute_update_description(
        {"_id": 1, "a": [{"x": 1}, {"x": 2}]},
        {"_id": 1, "a": [{"x": 1}, {"x": 99}]},
        {"$set": {"a.1.x": 99}},
    )
    assert diff["updatedFields"] == {"a.1.x": 99}
    assert diff["truncatedArrays"] == []


def test_push_reports_the_appended_indices() -> None:
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2]},
        {"_id": 1, "a": [1, 2, 8, 9]},
        {"$push": {"a": {"$each": [8, 9]}}},
    )
    assert diff["updatedFields"] == {"a.2": 8, "a.3": 9}
    assert diff["truncatedArrays"] == []


def test_set_beyond_the_end_reports_only_the_named_index() -> None:
    """`$set: {"a.5": 7}` on a 3-array leaves nulls at 3 and 4; mongod reports
    neither -- only the index the update named."""
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2, 3]},
        {"_id": 1, "a": [1, 2, 3, None, None, 7]},
        {"$set": {"a.5": 7}},
    )
    assert diff["updatedFields"] == {"a.5": 7}
    assert diff["truncatedArrays"] == []


def test_whole_field_set_is_wholesale_even_when_it_only_appends() -> None:
    """The case that proves this cannot be done from values alone: the same
    document, reached by `$set` rather than `$push`, is reported wholesale."""
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2]},
        {"_id": 1, "a": [1, 2, 3]},
        {"$set": {"a": [1, 2, 3]}},
    )
    assert diff["updatedFields"] == {"a": [1, 2, 3]}


def test_sliced_push_is_wholesale_not_an_append() -> None:
    """`$slice` truncates, so the append shortcut must not apply."""
    diff = compute_update_description(
        {"_id": 1, "a": [1, 2, 3]},
        {"_id": 1, "a": [1, 2]},
        {"$push": {"a": {"$each": [], "$slice": 2}}},
    )
    assert diff["updatedFields"] == {"a": [1, 2]}
    assert diff["truncatedArrays"] == []


def test_diff_array_grew_reports_the_appended_indices() -> None:
    """Growth used to wholesale-replace here, on the reasoning that "our
    simpler model" should make consumers re-fetch. Measured on mongod 8.2.11,
    growth is reported positionally -- for a pipeline update and for a `$push`
    alike. Only a whole-field operator `$set` resends the array."""
    diff = compute_update_description({"_id": 1, "a": [1]}, {"_id": 1, "a": [1, 2, 3]})
    assert diff["updatedFields"] == {"a.1": 2, "a.2": 3}
    assert diff["truncatedArrays"] == []

    whole = compute_update_description(
        {"_id": 1, "a": [1]}, {"_id": 1, "a": [1, 2, 3]}, {"$set": {"a": [1, 2, 3]}}
    )
    assert whole["updatedFields"] == {"a": [1, 2, 3]}


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

    # Element-wise, so the update spec is supplied -- see the array section.
    out = compute_update_description({"a": [{"1": 1}]}, {"a": [{"1": 2}]}, {"$set": {"a.0.1": 2}})
    assert out["updatedFields"] == {"a.0.1": 2}
    assert out["disambiguatedPaths"] == {"a.0.1": ["a", 0, "1"]}


def test_disambiguated_paths_absent_when_unambiguous() -> None:
    out = compute_update_description({"a": {"b": 1}}, {"a": {"b": 2}})
    assert "disambiguatedPaths" not in out

    # Plain array index paths are NOT ambiguous on their own.
    out = compute_update_description({"a": [1, 2]}, {"a": [9, 2]}, {"$set": {"a.0": 9}})
    assert out["updatedFields"] == {"a.0": 9}
    assert "disambiguatedPaths" not in out


def test_disambiguated_paths_for_removed_and_wholesale_arrays() -> None:
    out = compute_update_description({"a": {"1": 1}, "b": 0}, {"b": 0})
    assert out["removedFields"] == ["a"]
    assert "disambiguatedPaths" not in out  # "a" itself is unambiguous

    # A shrink is reported wholesale at ``a.2``, and that path still carries a
    # numeric-string FIELD name, so it is still disambiguated.
    out = compute_update_description(
        {"a": {"2": [1, 2, 3]}}, {"a": {"2": [1]}}, {"$pop": {"a.2": 1}}
    )
    assert out["updatedFields"] == {"a.2": [1]}
    assert out["truncatedArrays"] == []
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
