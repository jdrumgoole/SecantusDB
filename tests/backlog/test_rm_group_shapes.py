"""`$group` / `$bucket` / `$bucketAuto` / `$sortByCount` shapes found on
2026-10-22 by `tools/probes/group_shapes.py` over the lines of
`secantus-core/src/group.rs` that no suite executed. Each test runs pipelines
on mongod live and on the Rust server and compares the answers as canonical
extended JSON (so a value of the wrong BSON type is a difference), or the
error's code and message.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import Decimal128, json_util
from bson.json_util import JSONMode, JSONOptions

from tests.backlog.conftest import mongo_answer

_CANON = JSONOptions(json_mode=JSONMode.CANONICAL)
#: A stage mongod cannot push into SBE, so the `$group` after it runs in the
#: classic engine (RM-280 is the difference between the two).
_CLASSIC = [{"$addFields": {"zz": 1}}]


def _answer(client: Any, pipeline: list, docs: list, ordered: bool) -> Any:
    coll = client.rm_group_shapes.c
    coll.drop()
    client.rm_group_shapes.create_collection("c")
    if docs:
        coll.insert_many(docs)
    got = mongo_answer(
        lambda: [json_util.dumps(d, json_options=_CANON) for d in coll.aggregate(pipeline)]
    )
    if got[0] == "ok" and not ordered:
        return ("ok", sorted(got[1]))
    return got


def _diverging(rust_mongo: Any, mongod: Any, shapes: list, ordered: bool = False) -> list:
    out = []
    for label, pipeline, docs in shapes:
        ours = _answer(rust_mongo, pipeline, docs, ordered)
        theirs = _answer(mongod, pipeline, docs, ordered)
        if ours != theirs:
            out.append(f"{label}:\n  mongod {theirs!r}\n  rust   {ours!r}")
    return out


def _g(**accs: Any) -> list:
    return [{"$group": {"_id": None, **accs}}]


def _vals(values: list) -> list:
    return [{"_id": i, "x": v} for i, v in enumerate(values)]


@pytest.mark.backlog_fixed("RM-275")
def test_group_spec_parse_errors(rust_mongo: Any, mongod: Any) -> None:
    """**A malformed `$group` field was answered "not supported", or run.**
    mongod parses each output field as an accumulator statement: an object
    naming an operator (40234), no `.` (40235) or leading `$` (40236) in the
    name, one operator (40238), not an array argument (40237), a known
    accumulator (15952), `$count` with no arguments (14), and an expression
    object with one operator field (15983) -- all before any document is
    read (measured 8.2.11, 2026-10-22)."""
    one = _vals([1])
    shapes = [
        ("not accumulator", _g(c=1), one),
        ("empty accumulator", _g(c={}), one),
        ("dotted field", [{"$group": {"_id": None, "a.b": {"$sum": 1}}}], one),
        ("dollar field", [{"$group": {"_id": None, "$a": {"$sum": 1}}}], one),
        ("two accumulators", _g(c={"$sum": 1, "$avg": 1}), one),
        ("array argument", _g(c={"$sum": ["$x", 1]}), one),
        ("unknown accumulator", _g(c={"$bogus": 1}), one),
        ("unknown accumulator, empty collection", _g(c={"$bogus": 1}), []),
        ("$count with argument", _g(c={"$count": {"x": 1}}), one),
        ("_id two operators", [{"$group": {"_id": {"$add": [1], "$b": 2}}}], one),
        ("accumulator argument two operators", _g(s={"$sum": {"$add": [1], "$b": 2}}), one),
        ("$sortByCount two operators", [{"$sortByCount": {"$a": 1, "$b": 2}}], one),
        (
            "$bucket output, empty collection",
            [{"$bucket": {"groupBy": "$x", "boundaries": [0, 1], "output": {"a": 1}}}],
            [],
        ),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-276")
def test_n_accumulator_arguments(rust_mongo: Any, mongod: Any) -> None:
    """**`$top` / `$topN` / `$firstN` and the rest refused a bad argument as
    "not supported", or ran it.** mongod checks each one's fields as it
    parses (5788001-5, 5787801/5787900-7, 10065, 15974, 15975), and its
    classic engine checks `n` when a group starts -- evaluated against an
    EMPTY document, so `n: "$_id"` is missing (5787902/3/8) -- while a
    constant NaN fails the pipeline's optimization (31109) (measured
    8.2.11, 2026-10-22)."""
    docs = _vals([3, 1, 2])
    srt = {"_id": 1}
    shapes = [
        ("topN missing n", _g(t={"$topN": {"output": "$x", "sortBy": srt}}), docs),
        ("topN missing sortBy", _g(t={"$topN": {"n": 1, "output": "$x"}}), docs),
        ("topN missing output", _g(t={"$topN": {"n": 1, "sortBy": srt}}), docs),
        (
            "topN unknown field",
            _g(t={"$topN": {"n": 1, "output": "$x", "sortBy": srt, "z": 1}}),
            docs,
        ),
        ("top with n", _g(t={"$top": {"n": 1, "output": "$x", "sortBy": srt}}), docs),
        ("sortBy not object", _g(t={"$topN": {"n": 1, "output": "$x", "sortBy": 1}}), docs),
        (
            "sortBy direction 2",
            _g(t={"$topN": {"n": 1, "output": "$x", "sortBy": {"_id": 2}}}),
            docs,
        ),
        (
            "sortBy direction string",
            _g(t={"$topN": {"n": 1, "output": "$x", "sortBy": {"_id": "a"}}}),
            docs,
        ),
        ("firstN not object", _g(t={"$firstN": 2}), docs),
        ("minN unknown field", _g(t={"$minN": {"n": 1, "input": "$x", "z": 1}}), docs),
        ("firstN missing input", _g(t={"$firstN": {"n": 2}}), docs),
        ("n decimal", _g(t={"$topN": {"n": Decimal128("2"), "output": "$x", "sortBy": srt}}), docs),
        (
            "classic n zero",
            _CLASSIC + _g(t={"$topN": {"n": 0, "output": "$x", "sortBy": srt}}),
            docs,
        ),
        ("classic n fraction", _CLASSIC + _g(t={"$firstN": {"n": 2.5, "input": "$x"}}), docs),
        ("classic n string", _CLASSIC + _g(t={"$firstN": {"n": "2", "input": "$x"}}), docs),
        ("classic n group key", _CLASSIC + _g(t={"$firstN": {"n": "$_id", "input": "$x"}}), docs),
        ("classic n nan", _CLASSIC + _g(t={"$firstN": {"n": float("nan"), "input": "$x"}}), docs),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-277")
def test_group_key_document_field_order(rust_mongo: Any, mongod: Any) -> None:
    """**Documents with the same fields in another order were grouped
    together.** mongod keys `{a: 1, b: 2}` and `{b: 2, a: 1}` apart, while
    `{a: 1.0, b: 2}` joins `{a: 1, b: 2}` (measured 8.2.11, 2026-10-22)."""
    shapes = [
        (
            "field order",
            [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
            _vals([{"a": 1, "b": 2}, {"b": 2, "a": 1}, {"a": 1.0, "b": 2}]),
        ),
        (
            "nested field order",
            [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
            _vals([{"o": {"a": 1, "b": 2}}, {"o": {"b": 2, "a": 1}}]),
        ),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-278")
def test_bucket_auto(rust_mongo: Any, mongod: Any) -> None:
    """**`$bucketAuto` sized its buckets differently, and answered a bad
    argument "not supported".** mongod fixes the bucket size once, as the
    document count over the bucket count ROUNDED, and the last bucket takes
    the rest (10 into 3 is 3/3/4); its arguments are checked in the order
    written (40239-40246, 40257, 40261) (measured 8.2.11, 2026-10-22)."""
    ten = _vals(list(range(10)))
    shapes = [
        ("10 into 3", [{"$bucketAuto": {"groupBy": "$x", "buckets": 3}}], ten),
        ("11 into 4", [{"$bucketAuto": {"groupBy": "$x", "buckets": 4}}], _vals(list(range(11)))),
        ("8 into 3", [{"$bucketAuto": {"groupBy": "$x", "buckets": 3}}], _vals(list(range(8)))),
        ("not object", [{"$bucketAuto": 1}], ten),
        ("groupBy not a path", [{"$bucketAuto": {"groupBy": "x", "buckets": 2}}], ten),
        ("buckets string", [{"$bucketAuto": {"groupBy": "$x", "buckets": "2"}}], ten),
        ("buckets fraction", [{"$bucketAuto": {"groupBy": "$x", "buckets": 2.5}}], ten),
        ("buckets zero", [{"$bucketAuto": {"groupBy": "$x", "buckets": 0}}], ten),
        (
            "unknown granularity",
            [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": "R7"}}],
            ten,
        ),
        (
            "granularity number",
            [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": 5}}],
            ten,
        ),
        ("unknown option", [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "z": 1}}], ten),
        ("missing buckets", [{"$bucketAuto": {"groupBy": "$x"}}], ten),
        ("output not object", [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "output": 1}}], ten),
    ]
    assert _diverging(rust_mongo, mongod, shapes, ordered=True) == []


@pytest.mark.backlog_fixed("RM-279")
def test_accumulators_new_and_refusing(rust_mongo: Any, mongod: Any) -> None:
    """**`$concatArrays` and `$setUnion` were unknown accumulators, and
    `$mergeObjects` answered a non-object "not supported".** mongod 8.2 has
    both array accumulators (a missing value skipped, anything else not an
    array a TypeMismatch), and its classic engine refuses a non-object
    `$mergeObjects` input with 40400 (measured 8.2.11, 2026-10-22)."""
    shapes = [
        (
            "$concatArrays",
            _g(c={"$concatArrays": "$x"}),
            [{"_id": 1, "x": [1]}, {"_id": 2}, {"_id": 3, "x": [[2], 3]}],
        ),
        ("$concatArrays null", _CLASSIC + _g(c={"$concatArrays": "$x"}), _vals([[1], None])),
        ("$setUnion one group", _g(c={"$setUnion": "$x"}), _vals([[1], [1.0]])),
        ("$setUnion not array", _CLASSIC + _g(c={"$setUnion": "$x"}), _vals([[1], 5])),
        (
            "$mergeObjects not object",
            _CLASSIC + _g(m={"$mergeObjects": "$x"}),
            _vals([{"a": 1}, 5]),
        ),
        ("$mergeObjects null", _g(m={"$mergeObjects": "$x"}), _vals([None, {"a": 1}])),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog("RM-280")
def test_group_in_sbe_error_codes(rust_mongo: Any, mongod: Any) -> None:
    """A `$group` mongod pushes into its SBE engine -- one after only
    `$match` / `$sort` / `$limit` / `$project` -- fails with SBE's codes:
    7548606 / 7548607 for `n`, 5911200 for `$mergeObjects`, 7157705 for `$ln`
    in the key (measured 8.2.11, 2026-10-22)."""
    docs = _vals([1, 2])
    shapes = [
        ("n zero", _g(t={"$firstN": {"n": 0, "input": "$x"}}), docs),
        ("$mergeObjects", _g(m={"$mergeObjects": "$x"}), docs),
        ("$ln key", [{"$group": {"_id": {"$ln": "abc"}}}], docs),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog("RM-281")
def test_expression_object_with_two_operators(rust_mongo: Any, mongod: Any) -> None:
    """An expression object whose first field names an operator may have no
    other field: 15983 in `$addFields`, `$set` and `$expr`, 16410 in
    `$project`. The Rust server reads such an object as a literal document
    (measured 8.2.11, 2026-10-22)."""
    docs = _vals([1])
    two = {"$add": [1], "$b": 2}
    shapes = [
        ("$addFields", [{"$addFields": {"y": two}}], docs),
        ("$project", [{"$project": {"y": two}}], docs),
        ("$match $expr", [{"$match": {"$expr": {"$eq": [two, 1]}}}], docs),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []
