"""Gap tests for the window-function divergences found on 2026-10-21, when
`tests/mongod_live/test_window_functions.py` first compared `$setWindowFields`
with mongod 8.2.11 (nothing in CI had: `windowfields.rs` had no unit tests,
and its only comparison was the retired Rust-vs-Python parity suite)."""

from __future__ import annotations

from typing import Any

import pytest
from bson import Decimal128, json_util

from tests.backlog.conftest import mongo_answer

_CANONICAL = json_util.JSONOptions(json_mode=json_util.JSONMode.CANONICAL)


def _answers(rust_mongo: Any, mongod: Any, docs: list, pipeline: list) -> tuple[str, str]:
    """Both servers' answers to `pipeline` over `docs`, as canonical extended
    JSON so a type or field-order difference is a difference."""
    out = []
    for client in (rust_mongo, mongod):
        coll = client["rm_found_1021"]["w"]
        coll.drop()
        coll.insert_many(docs)
        out.append(
            json_util.dumps(
                mongo_answer(lambda c=coll: list(c.aggregate(pipeline))), json_options=_CANONICAL
            )
        )
    return out[0], out[1]


_EMA_DOCS = [
    {"_id": 1, "x": 10},
    {"_id": 2, "x": None},
    {"_id": 3},
    {"_id": 4, "x": 4},
    {"_id": 5, "x": Decimal128("2.5")},
]


@pytest.mark.backlog("RM-225")
@pytest.mark.parametrize("spec", [{"N": 2}, {"alpha": 0.75}], ids=["N", "alpha"])
def test_exp_moving_avg_over_null_missing_and_decimal(
    rust_mongo: Any, mongod: Any, spec: dict
) -> None:
    """mongod 8.2.11 averages over a null, a missing and a Decimal128 input
    (the null and missing rows answer null and do not move the average; a
    Decimal128 input turns the average decimal). The Rust server answers
    2 "not supported by the Rust server" for all of it."""
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {"e": {"$expMovingAvg": {"input": "$x", **spec}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    ours, theirs = _answers(rust_mongo, mongod, _EMA_DOCS, pipeline)
    assert ours == theirs


_SPEC_DOCS = [{"_id": i, "a": i, "b": i % 2, "y": i * 2} for i in range(1, 5)] + [
    {"_id": 5, "a": None, "b": 0, "y": 1}
]

#: (name, a `$setWindowFields` spec mongod refuses, with its own code).
_BAD_SPECS = [
    ("linearFill without a sortBy", {"output": {"f": {"$linearFill": "$y"}}}),
    (
        "shift with a window",
        {
            "sortBy": {"a": 1},
            "output": {
                "s": {
                    "$shift": {"output": "$y", "by": 1},
                    "window": {"documents": ["unbounded", "current"]},
                }
            },
        },
    ),
    (
        "range bounds with two sort keys",
        {"sortBy": {"a": 1, "b": 1}, "output": {"r": {"$sum": "$y", "window": {"range": [-1, 1]}}}},
    ),
    (
        "range window over a null sort value",
        {"sortBy": {"a": 1}, "output": {"r": {"$sum": "$y", "window": {"range": [-1, 1]}}}},
    ),
]


@pytest.mark.backlog("RM-226")
@pytest.mark.parametrize(("name", "stage"), _BAD_SPECS, ids=[n for n, _ in _BAD_SPECS])
def test_window_spec_errors_carry_mongods_codes(
    rust_mongo: Any, mongod: Any, name: str, stage: dict
) -> None:
    """mongod 8.2.11 refuses each spec with a code of its own: 605001,
    9 ("$shift does not accept a 'window' field"), 5339902 and 5429414. The
    Rust server answers 2 "not supported by the Rust server" for all four."""
    ours, theirs = _answers(rust_mongo, mongod, _SPEC_DOCS, [{"$setWindowFields": stage}])
    assert ours == theirs


_SHIFT_DOCS = [{"_id": 1, "x": 1}, {"_id": 2}, {"_id": 3, "x": None}, {"_id": 4, "x": 4}]


@pytest.mark.backlog_fixed("RM-227")
@pytest.mark.parametrize(
    "spec",
    [
        {"output": "$x", "by": -1, "default": "D"},
        {"output": "$x", "by": -1},
        {"output": {"$getField": "nosuch"}, "by": 1, "default": "D"},
        {"output": "$x", "by": 0, "default": "D"},
    ],
    ids=["absent field", "no default", "missing expression", "by 0"],
)
def test_shift_default_replaces_a_missing_output(rust_mongo: Any, mongod: Any, spec: dict) -> None:
    """mongod 8.2.11: when `output` evaluates to MISSING on the shifted row,
    `$shift` answers its `default` (null without one); an explicit null stays
    null. The Rust server answered null for a missing output."""
    pipeline = [
        {"$setWindowFields": {"sortBy": {"_id": 1}, "output": {"s": {"$shift": spec}}}},
        {"$project": {"s": 1}},
    ]
    ours, theirs = _answers(rust_mongo, mongod, _SHIFT_DOCS, pipeline)
    assert ours == theirs


#: `$convert` targets mongod refuses or answers with `onError`, each with and
#: without an `onError` (measured on mongod 8.2.11, 2026-10-21).
_CONVERT_TARGETS = ["uuid", "foo", "array", "timestamp", "null", "regex", 99, 4, {"type": "uuid"}]


@pytest.mark.backlog("RM-228")
@pytest.mark.parametrize("on_error", [False, True], ids=["plain", "onError"])
@pytest.mark.parametrize("to", _CONVERT_TARGETS, ids=[repr(t) for t in _CONVERT_TARGETS])
def test_convert_to_a_target_it_cannot_produce(
    rust_mongo: Any, mongod: Any, to: Any, on_error: bool
) -> None:
    """mongod 8.2.11: an unknown type name answers 2 "Unknown type name: X"
    and a number that is no BSON type 9, `onError` or not; a real BSON type
    `$convert` cannot produce answers 241 "Unsupported conversion from string
    to array in $convert with no onError value", or the `onError` value when
    one is given. The Rust server answers 2 "not supported by the Rust
    server" for every one of them."""
    spec: dict[str, Any] = {"input": "$x", "to": to}
    if on_error:
        spec["onError"] = "E"
    pipeline = [{"$project": {"_id": 0, "r": {"$convert": spec}}}]
    ours, theirs = _answers(rust_mongo, mongod, [{"_id": 1, "x": "5"}], pipeline)
    assert ours == theirs
