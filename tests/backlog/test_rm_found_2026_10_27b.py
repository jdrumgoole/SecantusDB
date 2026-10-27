"""Gap tests for what was found on 2026-10-27 while making every `find`
cursor read on as mongod's does (RM-080): two refusals worded or decided
otherwise than mongod's (fixed in the same batch), and the cursors that
still read everything at the command.

The reference is mongod 8.2.11 (`mongod`); the Rust server is
`secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import datetime
import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _coll(client: Any) -> Any:
    return client["rm_found_1027b"]["c" + uuid.uuid4().hex[:10]]


def _min_max_refusals(client: Any) -> list:
    c = _coll(client)
    c.insert_many([{"_id": i, "x": i % 5, "y": i} for i in range(20)])
    c.create_index([("x", 1), ("y", 1)])
    db = c.database
    out = []
    for extra in (
        {"min": {"x": 2}, "hint": "x_1_y_1"},
        {"max": {"x": 4}, "hint": "x_1_y_1", "skip": 1, "limit": 3, "sort": {"x": 1}},
        {"min": {"x": 2, "y": 0}, "max": {"x": 2, "y": 0}, "hint": "x_1_y_1"},
        {"min": {"x": 2, "y": 0}},
        {"min": {"x": 2, "y": 0}, "hint": {"$natural": 1}},
        {"min": {"x": 2, "y": 0}, "hint": "x_1_y_1"},
    ):
        command = {"find": c.name, "filter": {"y": {"$gt": 1}}, **extra}
        answer = mongo_answer(lambda command=command: db.command(command))
        if answer[0] == "ok":
            answer = ("ok", [d["_id"] for d in answer[1]["cursor"]["firstBatch"]])
        out.append(answer[:2] + tuple(a.replace(c.name, "C") for a in answer[2:]))
    return out


@pytest.mark.backlog_fixed("RM-345")
def test_min_and_max_must_name_the_whole_index_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses `min` / `max` that name only a prefix of the
    hinted index's key (51174), and words each `min` / `max` refusal as its
    planner does, with the query it was asked."""
    got, want = _min_max_refusals(rust_mongo), _min_max_refusals(mongod)
    assert want[0][:2] == ("err", 51174)
    assert got == want


def _planner_text_with_batch_size(client: Any) -> list:
    c = _coll(client)
    c.insert_one({"_id": 1})
    db = c.database
    out = []
    for extra in ({"batchSize": 3}, {"batchSize": 0, "limit": 2, "skip": 1}, {}):
        command = {"find": c.name, "filter": {"y": 1}, "hint": "nope", **extra}
        answer = mongo_answer(lambda command=command: db.command(command))
        out.append(answer[:2] + tuple(a.replace(c.name, "C") for a in answer[2:]))
    return out


@pytest.mark.backlog_fixed("RM-346")
def test_the_planners_words_name_the_batch_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 prints a `find`'s `batchSize`, whenever it is given,
    in the query its planner refuses: `ns=db.c batchSize=0 limit=2 skip=1Tree: ...`."""
    got, want = _planner_text_with_batch_size(rust_mongo), _planner_text_with_batch_size(mongod)
    assert "batchSize=0 limit=2 skip=1Tree" in want[1][2]
    assert got == want


def _near_reads(client: Any) -> dict:
    """A `$near` cursor and a `$geoNear` aggregate, written to after their
    first batch: what the rest of each returns."""
    out = {}
    c = _coll(client)
    c.insert_many([{"_id": i, "p": {"type": "Point", "coordinates": [i, 0]}} for i in range(6)])
    c.create_index([("p", "2dsphere")])
    near = {"p": {"$near": {"$geometry": {"type": "Point", "coordinates": [0, 0]}}}}
    cur = c.find(near).batch_size(1)
    first = [next(cur)["_id"]]
    c.insert_one({"_id": 20, "p": {"type": "Point", "coordinates": [0.5, 0]}})
    c.insert_one({"_id": 21, "p": {"type": "Point", "coordinates": [4.5, 0]}})
    c.delete_one({"_id": 3})
    out["find $near"] = first + [d["_id"] for d in cur]
    c = _coll(client)
    c.insert_many(
        [{"_id": i, "p": {"type": "Point", "coordinates": [i * 0.01, 0]}} for i in range(300)]
    )
    c.create_index([("p", "2dsphere")])
    pipeline = [
        {"$geoNear": {"near": {"type": "Point", "coordinates": [0, 0]}, "distanceField": "d"}}
    ]
    cur = c.aggregate(pipeline, batchSize=1)
    first = [next(cur)["_id"]]
    c.delete_many({})
    out["aggregate $geoNear, documents returned"] = len(first + [d["_id"] for d in cur])
    return out


@pytest.mark.backlog("RM-347")
def test_a_near_read_goes_ring_by_ring(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads a `$near` / `$geoNear` ring by ring as the cursor
    is read: a document inserted in a ring not yet searched is returned, one
    deleted from it is not; the ring searched buffers its documents."""
    got, want = _near_reads(rust_mongo), _near_reads(mongod)
    assert want["find $near"] == [0, 20, 1, 2, 4, 21, 5]
    assert got == want


def _time_series_reads(client: Any) -> dict:
    db = client["rm_found_1027b"]
    out = {}
    for many in (300, 4000):
        name = "ts" + uuid.uuid4().hex[:8]
        db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
        c = db[name]
        base = datetime.datetime(2024, 1, 1)
        c.insert_many(
            [{"t": base + datetime.timedelta(seconds=i), "m": i % 50, "v": i} for i in range(many)]
        )
        cur = c.aggregate([{"$match": {"v": {"$gte": 0}}}], batchSize=1)
        first = [next(cur)["v"]]
        c.delete_many({"m": {"$gte": 0}})
        rest = mongo_answer(lambda cur=cur: [d["v"] for d in cur])
        out[many] = (len(first), rest[0], len(rest[1]) if rest[0] == "ok" else rest[1])
    return out


@pytest.mark.backlog("RM-348")
def test_a_time_series_read_goes_bucket_by_bucket(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads a time-series collection's buckets through its
    cursor stage, 32 buckets first and twice as many at each refill, each
    bucket unpacked as the cursor reaches it: what a getMore returns after a
    delete depends on how many buckets were buffered."""
    got, want = _time_series_reads(rust_mongo), _time_series_reads(mongod)
    assert want[300][1] == "ok"
    assert got == want


def _trial_reads(client: Any) -> int:
    c = _coll(client)
    c.insert_many([{"_id": i, "a": i % 7, "b": i % 11} for i in range(3000)])
    c.create_index("a")
    c.create_index("b")
    cur = c.find({"a": {"$gte": 2}, "b": {"$gte": 3}}).batch_size(2)
    first = [next(cur)["_id"], next(cur)["_id"]]
    c.delete_many({})
    return len(first + [d["_id"] for d in cur])


@pytest.mark.backlog("RM-349")
def test_a_plan_mongod_tried_buffers_only_its_trial(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 picks between two usable indexes by a trial that stops
    at 101 results, returns those it buffered, then reads on: after every
    document is deleted the cursor returns 101 in all."""
    want = _trial_reads(mongod)
    assert want == 101
    assert _trial_reads(rust_mongo) == want
