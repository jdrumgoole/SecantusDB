"""`$group` buckets every NaN together, measured against mongod 8.2.11.

Python dicts key a NaN by IDENTITY -- `hash(nan)` is 0 but `nan != nan`, so a
NaN is hashable and never matches itself on lookup. `$group` therefore gave
every NaN document its own bucket: two documents with `a: NaN` came back as two
buckets of 1 where mongod reports one of 2. A wrong aggregation result, not an
error, so nothing surfaced it.

mongod also merges a double NaN with a `Decimal128` NaN into that same bucket,
and applies the rule inside arrays and subdocuments (probed 2026-09-05).
"""

from __future__ import annotations

import pytest
from bson import Decimal128, Int64

from secantus.aggregate import PipelineContext, apply_pipeline

NAN = float("nan")
GROUP = [{"$group": {"_id": "$a", "n": {"$sum": 1}}}]


def _buckets(docs, pipeline=None):
    out = apply_pipeline([dict(d) for d in docs], pipeline or GROUP, PipelineContext(None, "t"))
    return sorted(o["n"] for o in out)


@pytest.mark.parametrize(
    ("docs", "expected"),
    [
        # Every NaN is one bucket.
        ([{"a": NAN}, {"a": NAN}], [2]),
        ([{"a": NAN}, {"a": NAN}, {"a": NAN}], [3]),
        # ...including across numeric types.
        ([{"a": NAN}, {"a": Decimal128("NaN")}], [2]),
        # ...and nested.
        ([{"a": [NAN]}, {"a": [NAN]}], [2]),
        ([{"a": {"k": NAN}}, {"a": {"k": NAN}}], [2]),
        # A NaN does not swallow non-NaN values.
        ([{"a": NAN}, {"a": 1}, {"a": NAN}], [1, 2]),
        # The rules that already held, as a guard against over-merging.
        ([{"a": 1}, {"a": 1.0}], [2]),
        ([{"a": 1}, {"a": Int64(1)}], [2]),
        ([{"a": 0.0}, {"a": -0.0}], [2]),
        ([{"a": float("inf")}, {"a": float("inf")}], [2]),
        ([{"a": 1}, {"a": 2}], [1, 1]),
        ([{"a": "x"}, {"a": "y"}], [1, 1]),
    ],
)
def test_group_buckets_nan_as_one_key(docs, expected):
    assert _buckets(docs) == expected


def test_sort_by_count_shares_the_rule():
    """A neighbour on the same key path -- it bucketed NaN separately too."""
    out = apply_pipeline(
        [{"a": NAN}, {"a": NAN}], [{"$sortByCount": "$a"}], PipelineContext(None, "t")
    )
    assert [o["count"] for o in out] == [2]


def test_a_compound_group_key_containing_nan():
    out = apply_pipeline(
        [{"a": NAN, "b": 1}, {"a": NAN, "b": 2}],
        [{"$group": {"_id": {"x": "$a"}, "n": {"$sum": 1}}}],
        PipelineContext(None, "t"),
    )
    assert [o["n"] for o in out] == [2]
