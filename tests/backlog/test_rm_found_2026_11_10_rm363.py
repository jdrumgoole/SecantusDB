"""Gap and regression tests for what was found on 2026-11-10 while making a
time-series `find` the aggregation mongod converts it to (RM-363, RM-455).

The reference is mongod 8.2.11 (`mongod`, `mongod_rs`); the Rust server is
`secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import datetime
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.opdebug import logged

#: The options of a `find` mongod refuses as it converts the find to an
#: aggregation, two at a time to pin the order it checks them in, and the
#: ones it lets through (`false`, `singleBatch` with `limit: 1`).
_OPTIONS = {
    "singleBatch": {"singleBatch": True, "limit": 2},
    "singleBatch, limit 1": {"singleBatch": True, "limit": 1},
    "singleBatch false": {"singleBatch": False},
    "returnKey false": {"returnKey": False},
    "min, then tailable": {"tailable": True, "min": {"x": 1}, "hint": {"x": 1}},
    "min, then returnKey": {"min": {"x": 1}, "returnKey": True, "hint": {"x": 1}},
    "returnKey, then tailable": {"returnKey": True, "tailable": True},
    "returnKey, then showRecordId": {"returnKey": True, "showRecordId": True},
    "showRecordId, then tailable": {"showRecordId": True, "tailable": True},
    "tailable, then noCursorTimeout": {"tailable": True, "noCursorTimeout": True},
    "noCursorTimeout, then allowPartialResults": {
        "noCursorTimeout": True,
        "allowPartialResults": True,
    },
    "allowPartialResults, then singleBatch": {"allowPartialResults": True, "singleBatch": True},
    "singleBatch, then readOnce": {"singleBatch": True, "readOnce": True},
    "readOnce": {"readOnce": True},
    "a $natural sort and singleBatch": {"sort": {"$natural": 1}, "singleBatch": True},
}


def _refusals(client: Any) -> dict:
    db = client["rm_found_1110_rm363"]
    client.drop_database(db.name)
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    db.ts.insert_one({"t": datetime.datetime(2024, 1, 1), "m": 1, "v": 1})
    db.create_collection("p")
    db.p.insert_one({"_id": 1, "x": 1})
    db.p.create_index("x")
    db.create_collection("v", viewOn="p", pipeline=[])

    def shape(r: Any) -> Any:
        c = r["cursor"]
        return len(c["firstBatch"]), c["id"] == 0

    answers = {}
    for coll in ("ts", "v"):
        for case, extra in _OPTIONS.items():
            cmd = {"find": coll, **extra}
            answers[coll, case] = mongo_answer(lambda cmd=cmd: shape(db.command(cmd)))
    client.drop_database(db.name)
    return answers


@pytest.mark.backlog_fixed("RM-497")
def test_a_find_converted_to_an_aggregation_refuses_what_an_aggregation_does_not_take(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 converts a `find` of a view or a time-series collection
    to an aggregation, and refuses `min`, `max`, `returnKey`,
    `showRecordId`, `tailable`, `noCursorTimeout`, `allowPartialResults`,
    `singleBatch` (but with `limit: 1`) and `readOnce`, in that order (168).
    This server took all but `tailable`, `min` and `max`, and checked
    `tailable` first."""
    want = _refusals(mongod)
    assert want["ts", "readOnce"][:2] == ("err", 168)
    got = _refusals(rust_mongo)
    assert {k: v for k, v in got.items() if v != want[k]} == {}


def _meta_read(db: Any) -> None:
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    db.ts.insert_many(
        [{"t": datetime.datetime(2024, 1, 1, 0, 0, i), "m": i % 3, "v": i} for i in range(30)]
    )


@pytest.mark.backlog_fixed("RM-496")
def test_a_read_of_the_meta_index_names_the_index_over_the_buckets(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for a `find` of a time-series collection read
    through its default meta-and-time index names the index as the buckets
    keep it: `IXSCAN { meta: 1, control.min.t: 1, control.max.t: 1 }`. This
    server's named it as `listIndexes` shows it, `IXSCAN { m: 1, t: 1 }`."""

    def summary(client: Any, is_mongod: bool) -> Any:
        lines, _ = logged(client, is_mongod, _meta_read, lambda d: list(d.ts.find({"m": 1})))
        return [a.get("planSummary") for _, a in lines if "find" in a.get("command", {})]

    want = summary(mongod_rs, True)
    assert want == ["IXSCAN { meta: 1, control.min.t: 1, control.max.t: 1 }"]
    assert summary(rust_mongo, False) == want


def _indexed_series(db: Any) -> None:
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    db.ts.insert_many(
        [
            {"t": datetime.datetime(2024, 1, 1, 0, 0, i), "m": {"a": i % 3, "b": 1}, "v": i}
            for i in range(30)
        ]
    )
    db.ts.create_index([("v", 1)])
    db.ts.create_index([("m.a", 1), ("t", -1)])
    db.ts.create_index([("m.b", -1)])


#: Reads of a time-series collection through each kind of index on it.
_INDEXED_READS = {
    "find on a meta subfield": lambda d: list(d.ts.find({"m.a": 1})),
    "find on a descending meta subfield": lambda d: list(d.ts.find({"m.b": 1})),
    "find on a measurement": lambda d: list(d.ts.find({"v": 3})),
    "hinted find": lambda d: list(d.ts.find({}).hint([("v", 1)])),
    "aggregate": lambda d: list(d.ts.aggregate([{"$match": {"m.a": 1}}])),
    "count": lambda d: d.ts.count_documents({"m.a": 1}),
    "distinct": lambda d: d.ts.distinct("v", {"m.a": 1}),
}


def _summaries(client: Any, is_mongod: bool, op: Any) -> Any:
    lines, profile = logged(client, is_mongod, _indexed_series, op)
    return (
        [a.get("planSummary") for _, a in lines],
        [p.get("planSummary") for p in profile],
    )


@pytest.mark.backlog_fixed("RM-496")
def test_every_read_of_a_time_series_index_names_it_over_the_buckets(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 names every index a time-series read uses as its
    buckets keep it, in the `Slow query` line and the profile entry alike:
    the meta field as `meta`, a measurement field as its pair of bounds,
    `control.max` first for a descending key (`IXSCAN { meta.a: 1,
    control.max.t: -1, control.min.t: -1 }` for `{m.a: 1, t: -1}`,
    `IXSCAN { control.min.v: 1, control.max.v: 1 }` for `{v: 1}`), for a
    `find`, an `aggregate`, a `count` and a `distinct`. This server named
    them as `listIndexes` shows them (measured 2026-11-13)."""
    want = {case: _summaries(mongod_rs, True, op) for case, op in _INDEXED_READS.items()}
    assert want["find on a meta subfield"] == (
        ["IXSCAN { meta.a: 1, control.max.t: -1, control.min.t: -1 }"],
        ["IXSCAN { meta.a: 1, control.max.t: -1, control.min.t: -1 }"],
    )
    got = {case: _summaries(rust_mongo, False, op) for case, op in _INDEXED_READS.items()}
    assert {k: v for k, v in got.items() if v != want[k]} == {}


@pytest.mark.backlog("RM-600")
def test_a_time_series_read_bounded_on_its_time_field_scans_the_clustered_buckets(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 reads a time-series collection bounded on its time
    field, with no index to serve the bound, as a `CLUSTERED_IXSCAN` of its
    buckets: they are clustered by `_id`, whose timestamp is each bucket's
    start, so the bound on `control.min.t` bounds the `_id`s read. This
    server's line says `COLLSCAN` (measured 2026-11-13)."""

    def summary(client: Any, is_mongod: bool) -> Any:
        lines, profile = logged(
            client,
            is_mongod,
            _meta_read,
            lambda d: list(d.ts.find({"t": {"$gt": datetime.datetime(2024, 1, 1)}})),
        )
        return [a.get("planSummary") for _, a in lines], [p.get("planSummary") for p in profile]

    want = summary(mongod_rs, True)
    assert want == (["CLUSTERED_IXSCAN"], ["CLUSTERED_IXSCAN"])
    assert summary(rust_mongo, False) == want


@pytest.mark.backlog("RM-498")
def test_a_tailable_find_of_a_collection_that_is_not_capped_prints_its_query(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 refuses a tailable `find` of a collection that is not
    capped (2) with the query it could not plan, as its planner prints it:
    `error processing query: ns=<db>.pTree: $and\\nSort: {}\\nProj: {}\\n
    tailable cursor requested on non capped collection`. This server's
    message has no query in it."""

    def answer(client: Any) -> tuple:
        db = client["rm_found_1110_rm363_tail"]
        client.drop_database(db.name)
        db.create_collection("p")
        try:
            return mongo_answer(lambda: db.command({"find": "p", "tailable": True}))
        finally:
            client.drop_database(db.name)

    want = answer(mongod)
    assert want[:2] == ("err", 2)
    assert "pTree" in want[2]
    assert answer(rust_mongo) == want
