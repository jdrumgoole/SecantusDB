"""RM-701's remainder, measured on mongod 8.2.11 on 2026-11-28 (macOS arm64),
with the method of `test_rm_found_2026_11_09_rm348.py`: 4,000 measurements in
50 series (100 buckets), take one with `batchSize(1)`, delete every
measurement, count what the cursor still returns: what mongod's read had
buffered. Each count below was the same in three runs on mongod.

What still reads the collection whole on this server (0.5.3-beta.199):

* a string filter under a collation (mongod leaves 1,023 for `s >= "s500"`,
  255 for `s < "s100"` beside `{v: 1}`, 1,151 for a string beside a meta
  filter; this server 1,999, 399 and 2,879);
* an `$expr` that is not one comparison with a constant, beside `{v: 1}`
  (`$and` of two comparisons: 63; a computed operand: 62; this server 99
  and 98).

A TTL index (with the partial filter on the meta field that mongod
requires), a 2dsphere index, `$where` and `$text` read as mongod does here,
so they are not tests.
"""

from __future__ import annotations

import datetime
import uuid
from collections.abc import Callable
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

_COLLATION = {"locale": "en", "strength": 2}


def _after_delete(client: Any, setup: Callable[[Any], None], read: Callable[[Any], Any]) -> int:
    db = client["rm_701_remainder"]
    name = "ts" + uuid.uuid4().hex[:8]
    db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
    c = db[name]
    base = datetime.datetime(2024, 1, 1)
    c.insert_many(
        [
            {
                "t": base + datetime.timedelta(seconds=i),
                "m": i % 50,
                "v": i,
                "w": (i * 7) % 4000,
                "s": f"S{i % 1000:03d}",
            }
            for i in range(4000)
        ]
    )
    setup(c)
    cur = read(c)
    next(cur)
    c.delete_many({"m": {"$gte": 0}})
    return len(list(cur))


def _nothing(c: Any) -> None:
    return None


def _index_v(c: Any) -> None:
    c.create_index("v")


def _collated(match: dict) -> Callable[[Any], Any]:
    return lambda c: c.aggregate([{"$match": match}], batchSize=1, collation=_COLLATION)


def _expr(expr: dict) -> Callable[[Any], Any]:
    return lambda c: c.aggregate([{"$match": {"$expr": expr}}], batchSize=1)


STRINGS = [
    ("s >= s500", _nothing, {"s": {"$gte": "s500"}}, 1023),
    ("s < s100 beside v_1", _index_v, {"s": {"$lt": "s100"}}, 255),
    ("s and meta", _nothing, {"s": {"$gte": "s100"}, "m": {"$lt": 40}}, 1151),
]


@pytest.mark.backlog("RM-701")
@pytest.mark.parametrize("setup,match,want", [s[1:] for s in STRINGS], ids=[s[0] for s in STRINGS])
def test_a_time_series_string_filter_under_a_collation_buffers_as_mongod(
    rust_mongo: Any, mongod: Any, setup: Any, match: dict, want: int
) -> None:
    assert _after_delete(mongod, setup, _collated(match)) == want
    assert _after_delete(rust_mongo, setup, _collated(match)) == want


EXPRS = [
    (
        "and of two",
        {"$and": [{"$gte": ["$v", 0]}, {"$lt": ["$v", 100]}]},
        63,
    ),
    ("computed operand", {"$lt": [{"$add": ["$v", 1]}, 100]}, 62),
]


@pytest.mark.backlog("RM-701")
@pytest.mark.parametrize("expr,want", [e[1:] for e in EXPRS], ids=[e[0] for e in EXPRS])
def test_a_time_series_expr_that_is_no_single_comparison_buffers_as_mongod(
    rust_mongo: Any, mongod: Any, expr: dict, want: int
) -> None:
    assert _after_delete(mongod, _index_v, _expr(expr)) == want
    assert _after_delete(rust_mongo, _index_v, _expr(expr)) == want


# A partial index of a time-series collection must be translatable to the
# buckets: mongod 8.2.11 refuses a filter it cannot, code 5916301 (a
# `$exists: false` is its `$not: {$exists: true}`, a `$ne` its `$not: {$eq}`),
# and an `$eq: null` with "can't handle comparison to null". This server
# accepts every one.

UNTRANSLATABLE = [
    {"v": {"$type": "number"}},
    {"v": {"$type": 2}},
    {"v": {"$exists": False}},
    {"v": {"$ne": 3}},
    {"v": {"$regex": "a"}},
    {"v": {"$eq": None}},
]


def _create_partial(client: Any, partial: dict) -> tuple:
    db = client["rm_701_remainder"]
    name = "ts" + uuid.uuid4().hex[:8]
    db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
    return mongo_answer(lambda: db[name].create_index("v", partialFilterExpression=partial))[:3]


@pytest.mark.backlog("RM-854")
@pytest.mark.parametrize("partial", UNTRANSLATABLE, ids=str)
def test_a_time_series_partial_index_must_translate_to_the_buckets(
    rust_mongo: Any, mongod: Any, partial: dict
) -> None:
    expected = _create_partial(mongod, partial)
    assert expected[:2] == ("err", 5916301) or "comparison to null" in expected[2], expected
    assert _create_partial(rust_mongo, partial) == expected
