"""Signed zero and numeric type in the WRITE path, measured against mongod 8.2.11.

Two questions that look like one, and mongod answers them differently:

* **equality** calls `0.0` and `-0.0` the same value — `{$eq: [0.0, -0.0]}` is
  true, `$cmp` is 0, and `find({a: -0.0})` matches a stored `0.0`;
* **change detection** does not — a `$set` of `-0.0` over `0.0` writes, reports
  `modifiedCount: 1`, and names the field in a change stream's `updatedFields`.

Conflating them meant `update_one({...}, {"$set": {"a": -0.0}})` on a stored
`0.0` did NOTHING: no write, no event, and a read-back of `0.0`. The value the
caller asked to store was silently not stored (2026-09-05).
"""

from __future__ import annotations

import math

import pytest
from bson import Decimal128, Int64

from secantus.diff import compute_update_description
from secantus.update import apply_update


def _is_negative_zero(v) -> bool:
    return isinstance(v, float) and v == 0.0 and math.copysign(1.0, v) < 0.0


@pytest.mark.parametrize(
    ("stored", "multiplier", "expect_negative_zero"),
    [
        # A stored DOUBLE zero keeps its own sign whatever the multiplier --
        # IEEE would flip it. Measured across negative, positive, zero and
        # fractional multipliers.
        (0.0, -1, False),
        (0.0, -1.0, False),
        (0.0, -2.5, False),
        (0.0, 5, False),
        (-0.0, -1, True),
        (-0.0, 5, True),
        (-0.0, 0.0, True),
    ],
)
def test_mul_does_not_flip_a_stored_zeros_sign(stored, multiplier, expect_negative_zero):
    got = apply_update({"a": stored}, {"$mul": {"a": multiplier}})["a"]
    assert got == 0.0
    assert _is_negative_zero(got) is expect_negative_zero, f"{stored} * {multiplier} -> {got!r}"


def test_mul_of_a_zero_still_writes_a_non_zero_result():
    """The rule is narrow: a NON-zero result writes normally."""
    got = apply_update({"a": 0.0}, {"$mul": {"a": float("inf")}})["a"]
    assert got != got, f"0.0 * inf should be NaN, got {got!r}"


def test_mul_of_a_decimal_zero_keeps_its_sign():
    assert str(apply_update({"a": Decimal128("0")}, {"$mul": {"a": -1}})["a"]) == "0"
    assert str(apply_update({"a": Decimal128("-0")}, {"$mul": {"a": -1}})["a"]) == "-0"


@pytest.mark.parametrize(
    ("pre", "post", "expected_field"),
    [
        # A signed-zero flip IS a change, bare and nested.
        ({"a": 0.0}, {"a": -0.0}, "a"),
        ({"a": -0.0}, {"a": 0.0}, "a"),
        ({"a": [0.0]}, {"a": [-0.0]}, None),  # array: reported, path varies
        ({"a": {"k": 0.0}}, {"a": {"k": -0.0}}, "a.k"),
        # ...and so is a numeric TYPE change. A Rust test asserted the opposite,
        # justified by `1 == 1.0`, which is Python's rule and not mongod's.
        ({"a": 1}, {"a": 1.0}, "a"),
        ({"a": 1.0}, {"a": 1}, "a"),
        ({"a": 1}, {"a": Int64(1)}, "a"),
    ],
)
def test_change_detection_sees_signed_zero_and_type(pre, post, expected_field):
    got = compute_update_description(pre, post)["updatedFields"]
    assert got, f"{pre} -> {post} reported no change"
    if expected_field is not None:
        assert expected_field in got, got


def test_change_detection_does_not_over_report():
    """The guard: an identical document still produces nothing."""
    same = {"a": 1.0, "b": [1, 2], "c": {"k": "v"}}
    out = compute_update_description(same, dict(same))
    assert out["updatedFields"] == {}
    assert out["removedFields"] == []
