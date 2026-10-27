"""Gap tests for the divergences found on 2026-10-25 while streaming the
rest of `aggregate` (RM-080): which `$group` mongod answers from a
DISTINCT_SCAN and in what order, how a `$unionWith` reads its collection,
and what a first batch of none leaves undone.

The reference is mongod 8.2.11, asked live (`mongod`); the Rust server is
`secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import json
import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _coll(client: Any) -> Any:
    return client["rm_found_1025"]["c" + uuid.uuid4().hex[:10]]


_DISTINCT_GROUPS = {
    "x_first": [{"$group": {"_id": "$x", "f": {"$first": "$y"}}}],
    "x_last": [{"$group": {"_id": "$x", "l": {"$last": "$y"}}}],
    "x_none": [{"$group": {"_id": "$x"}}],
    "sort_x_first": [{"$sort": {"x": 1}}, {"$group": {"_id": "$x", "f": {"$first": "$y"}}}],
    "sort_x_last": [{"$sort": {"x": 1}}, {"$group": {"_id": "$x", "l": {"$last": "$y"}}}],
    "sort_down_x_first": [{"$sort": {"x": -1}}, {"$group": {"_id": "$x", "f": {"$first": "$y"}}}],
    "sort_down_x_last": [{"$sort": {"x": -1}}, {"$group": {"_id": "$x", "l": {"$last": "$y"}}}],
    "sort_xy_first": [
        {"$sort": {"x": 1, "y": 1}},
        {"$group": {"_id": "$x", "f": {"$first": "$y"}}},
    ],
    "sort_xy_last": [
        {"$sort": {"x": 1, "y": 1}},
        {"$group": {"_id": "$x", "l": {"$last": "$y"}}},
    ],
    "id_last": [{"$group": {"_id": "$_id", "a": {"$last": "$y"}, "b": {"$last": "$x"}}}],
    "sort_id_last": [{"$sort": {"_id": 1}}, {"$group": {"_id": "$_id", "a": {"$last": "$y"}}}],
    "sort_down_id_last": [
        {"$sort": {"_id": -1}},
        {"$group": {"_id": "$_id", "a": {"$last": "$y"}}},
    ],
    "match_id_last": [
        {"$match": {"_id": {"$gt": 3}}},
        {"$group": {"_id": "$_id", "a": {"$last": "$y"}}},
    ],
    "id_covered": [{"$group": {"_id": "$_id"}}],
    "match_id_covered": [{"$match": {"_id": {"$gt": 3}}}, {"$group": {"_id": "$_id"}}],
}

_NOT_DISTINCT = {
    "x_first_and_last": [{"$group": {"_id": "$x", "f": {"$first": "$y"}, "l": {"$last": "$y"}}}],
    "sort_x_up_y_down_last": [
        {"$sort": {"x": 1, "y": -1}},
        {"$group": {"_id": "$x", "l": {"$last": "$y"}}},
    ],
}


def _distinct_groups(client: Any) -> dict:
    """Each `$group`'s winning plan under the stages it leaves, and its
    answer; for the shapes mongod does not rewrite, whether it did."""
    c = _coll(client)
    c.insert_many([{"_id": i, "x": i % 4, "y": i} for i in range(1, 13)])
    c.create_index("x")
    c.create_index([("x", 1), ("y", 1)])
    out = {}
    for name, pipeline in _DISTINCT_GROUPS.items():
        e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
        stages = e.get("stages") or [{"$cursor": e}]
        plan = stages[0]["$cursor"]["queryPlanner"]["winningPlan"]
        plan.pop("isCached", None)
        reply = c.database.command("aggregate", c.name, pipeline=pipeline, cursor={})
        out[name] = (plan, stages[1:], reply["cursor"]["firstBatch"])
    for name, pipeline in _NOT_DISTINCT.items():
        e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
        out[name] = "$groupByDistinctScan" in json.dumps(e, default=str)
    return out


@pytest.mark.backlog_fixed("RM-309")
def test_a_group_read_from_a_distinct_scan_is_planned_and_ordered_as_mongod_does(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 answers a `$group` of `$first`s, or of `$last`s, from a
    DISTINCT_SCAN, and returns the groups in the scan's order: a `$last`
    scans the other way (no sort: backward), a sort on the group's field
    and the accumulated one picks the compound index, the covered `_id`
    scan projects `{_id: 1}`. `$first` with `$last`, or a sort the index
    does not give, is a plain `$group`."""
    got, want = _distinct_groups(rust_mongo), _distinct_groups(mongod)
    scan = want["x_last"][0]
    while scan["stage"] != "DISTINCT_SCAN":
        scan = scan["inputStage"]
    assert scan["direction"] == "backward"
    assert [d["_id"] for d in want["x_last"][2]] == [3, 2, 1, 0]
    assert want["id_covered"][0]["transformBy"] == {"_id": 1}
    assert want["x_first_and_last"] is False
    assert got == want


def _unstreamed_distinct_groups(client: Any) -> dict:
    """The groups of a `$group` read from a DISTINCT_SCAN over a descending
    index, over a collated one with the command's collation, and through a
    view."""
    db = client["rm_found_1025"]
    out = {}
    pipelines = {
        "first": [{"$group": {"_id": "$x", "f": {"$first": "$y"}}}],
        "last": [{"$group": {"_id": "$x", "f": {"$last": "$y"}}}],
    }
    for name, key, options in (
        ("descending", [("x", -1)], {}),
        ("collated", [("x", 1)], {"collation": {"locale": "en"}}),
    ):
        c = _coll(client)
        c.insert_many([{"_id": i, "x": i % 4, "y": i} for i in range(1, 13)])
        c.create_index(key, **options)
        for kind, pipeline in pipelines.items():
            reply = db.command("aggregate", c.name, pipeline=pipeline, cursor={}, **options)
            out[f"{name} {kind}"] = reply["cursor"]["firstBatch"]
    c = _coll(client)
    c.insert_many([{"_id": i, "x": i % 4, "y": i} for i in range(1, 13)])
    c.create_index("x")
    view = "v" + uuid.uuid4().hex[:8]
    db.command("create", view, viewOn=c.name, pipeline=[])
    reply = db.command("aggregate", view, pipeline=pipelines["first"], cursor={})
    out["view first"] = reply["cursor"]["firstBatch"]
    return out


@pytest.mark.backlog("RM-309")
def test_a_distinct_scan_group_this_server_does_not_stream_keeps_the_scans_order(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 returns the groups in the DISTINCT_SCAN's order down a
    descending index and along a collated one."""
    keep = ("descending first", "descending last", "collated first", "collated last")

    def groups(client: Any) -> dict:
        found = _unstreamed_distinct_groups(client)
        return {k: found[k] for k in keep}

    got, want = groups(rust_mongo), groups(mongod)
    assert [d["_id"] for d in want["descending first"]] == [3, 2, 1, 0]
    assert got == want


@pytest.mark.backlog_fixed("RM-309")
def test_a_distinct_scan_group_through_a_view_keeps_the_scans_order(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a view's `$group` from the DISTINCT_SCAN of the
    collection beneath it, the groups in the scan's order."""
    got = _unstreamed_distinct_groups(rust_mongo)["view first"]
    want = _unstreamed_distinct_groups(mongod)["view first"]
    assert [d["_id"] for d in want] == [0, 1, 2, 3]
    assert got == want


def _union_reads(client: Any) -> dict:
    """What a `$unionWith` returns of its collection when that collection
    changes after the command, and after the cursor has reached it."""
    db = client["rm_found_1025"]
    c, other = _coll(client), _coll(client)
    c.insert_many([{"_id": i} for i in range(1, 201)])
    other.insert_many([{"_id": 1000 + i} for i in range(5)])
    reply = db.command(
        "aggregate", c.name, pipeline=[{"$unionWith": other.name}], cursor={"batchSize": 1}
    )
    other.delete_many({})
    other.insert_one({"_id": 5000})
    seen, cursor = [], reply["cursor"]["id"]
    while cursor:
        reply = db.command("getMore", cursor, collection=c.name)
        seen += [d["_id"] for d in reply["cursor"]["nextBatch"] if d["_id"] >= 1000]
        cursor = reply["cursor"]["id"]
    out = {"opened_when_reached": seen}
    c, other = _coll(client), _coll(client)
    c.insert_many([{"_id": i} for i in range(1, 6)])
    other.insert_many([{"_id": 1000 + i} for i in range(300)])
    reply = db.command(
        "aggregate", c.name, pipeline=[{"$unionWith": other.name}], cursor={"batchSize": 1}
    )
    reply = db.command("getMore", reply["cursor"]["id"], collection=c.name, batchSize=10)
    other.delete_many({})
    left, cursor = 0, reply["cursor"]["id"]
    while cursor:
        reply = db.command("getMore", cursor, collection=c.name)
        left += len(reply["cursor"]["nextBatch"])
        cursor = reply["cursor"]["id"]
    out["buffered_after_reaching"] = left
    return out


@pytest.mark.backlog_fixed("RM-080")
def test_a_union_with_reads_its_collection_when_the_cursor_reaches_it(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 opens a `$unionWith`'s collection only when the first
    one runs out, and reads it through a buffer of its own (32 documents,
    then doubling)."""
    got, want = _union_reads(rust_mongo), _union_reads(mongod)
    assert want == {"opened_when_reached": [5000], "buffered_after_reaching": 26}
    assert got == want


def _empty_first_batch(client: Any) -> dict:
    """What a pipeline asked for a first batch of none has done by the time
    the command returns: written its `$out`, met its error, counted."""
    db = client["rm_found_1025"]
    out = {}
    c, target = _coll(client), "out" + uuid.uuid4().hex[:8]
    c.insert_many([{"_id": i} for i in range(5)])
    db.command("aggregate", c.name, pipeline=[{"$out": target}], cursor={"batchSize": 0})
    out["out_written"] = db[target].count_documents({})
    c = _coll(client)
    c.insert_many([{"_id": i, "s": "abc" if i == 4 else str(i)} for i in range(5)])
    pipeline = [{"$group": {"_id": None, "t": {"$sum": {"$toInt": "$s"}}}}]
    command = mongo_answer(
        lambda: db.command("aggregate", c.name, pipeline=pipeline, cursor={"batchSize": 0})
    )
    out["error_at_command"] = command[0] == "err"
    if command[0] == "ok":
        more = mongo_answer(
            lambda: db.command("getMore", command[1]["cursor"]["id"], collection=c.name)
        )
        out["error_at_get_more"] = more[:2]
    c = _coll(client)
    c.insert_many([{"_id": i} for i in range(5)])
    reply = db.command("aggregate", c.name, pipeline=[{"$count": "n"}], cursor={"batchSize": 0})
    c.insert_one({"_id": 77})
    reply = db.command("getMore", reply["cursor"]["id"], collection=c.name)
    out["count"] = reply["cursor"]["nextBatch"]
    return out


@pytest.mark.backlog_fixed("RM-080")
def test_a_first_batch_of_none_runs_nothing_until_the_get_more(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 runs none of a pipeline asked for `batchSize: 0`: its
    `$out` is written, its error met and its documents read at the first
    `getMore`."""
    got, want = _empty_first_batch(rust_mongo), _empty_first_batch(mongod)
    assert want == {
        "out_written": 0,
        "error_at_command": False,
        "error_at_get_more": ("err", 241),
        "count": [{"n": 6}],
    }
    assert got == want


def _filled_batches(client: Any) -> dict:
    """The batch sizes of pipelines whose last batch fills exactly: read
    through the cursor stage, and one the planner reads whole."""
    db = client["rm_found_1025"]
    c, other = _coll(client), _coll(client)
    c.insert_many([{"_id": i} for i in range(40)])
    other.insert_many([{"_id": 1000 + i} for i in range(5)])
    out = {}
    for name, pipeline, size in (
        ("set", [{"$set": {"q": 1}}], 8),
        ("set_limit", [{"$set": {"q": 1}}, {"$limit": 16}], 8),
        ("union", [{"$unionWith": other.name}], 9),
        ("project", [{"$project": {"_id": 1}}], 8),
    ):
        reply = db.command("aggregate", c.name, pipeline=pipeline, cursor={"batchSize": size})
        sizes, cursor = [len(reply["cursor"]["firstBatch"])], reply["cursor"]["id"]
        while cursor:
            reply = db.command("getMore", cursor, collection=c.name, batchSize=size)
            sizes.append(len(reply["cursor"]["nextBatch"]))
            cursor = reply["cursor"]["id"]
        out[name] = sizes
    return out


@pytest.mark.backlog_fixed("RM-080")
def test_a_cursor_stage_that_fills_its_last_batch_exactly_stays_open(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 learns a pipeline is over only when it asks for another
    document: a last batch that fills exactly leaves the cursor open, and
    the next `getMore` is empty, through the cursor stage as through a read
    its planner takes whole."""
    got, want = _filled_batches(rust_mongo), _filled_batches(mongod)
    assert want["set"] == [8, 8, 8, 8, 8, 0]
    assert want["project"] == [8, 8, 8, 8, 8, 0]
    assert got == want


def _plan_brief(plan: dict) -> str:
    stages = []
    while plan:
        stages.append(plan["stage"] + (f"({plan['indexName']})" if "indexName" in plan else ""))
        plan = plan.get("inputStage", {})
    return ">".join(stages)


def _distinct_group_planners(client: Any, pipelines: dict) -> dict:
    """Each pipeline's parsed query, winning and rejected plans, and the
    stages after the read."""
    c = _coll(client)
    c.insert_many([{"_id": i, "x": i % 4, "y": i, "z": -i} for i in range(1, 25)])
    c.create_index("x")
    c.create_index([("x", 1), ("y", 1)])
    c.create_index([("x", 1), ("z", 1)])
    out = {}
    for name, pipeline in pipelines.items():
        e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
        planner = e["stages"][0]["$cursor"]["queryPlanner"]
        out[name] = (
            planner["parsedQuery"],
            _plan_brief(planner["winningPlan"]),
            [_plan_brief(p) for p in planner["rejectedPlans"]],
            [list(s)[0] for s in e["stages"][1:]],
        )
    return out


_MULTIPLANNED = {
    "sorted": [{"$sort": {"x": 1}}, {"$group": {"_id": "$x", "f": {"$first": "$y"}}}],
    "filtered": [{"$match": {"x": {"$gt": 0}}}, {"$group": {"_id": "$x", "f": {"$first": "$y"}}}],
    "uncovered": [{"$sort": {"x": 1}}, {"$group": {"_id": "$x", "f": {"$first": "$w"}}}],
}


@pytest.mark.backlog("RM-310")
def test_explain_lists_the_distinct_scans_a_group_tried(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 tries each index that could answer a sorted or
    filtered `$group` from a DISTINCT_SCAN and lists the losers in
    `rejectedPlans`."""
    got = _distinct_group_planners(rust_mongo, _MULTIPLANNED)
    want = _distinct_group_planners(mongod, _MULTIPLANNED)
    assert len(want["filtered"][2]) == 2
    assert got == want


_MATCH_AFTER_GROUP = {
    "on_the_key": [
        {"$group": {"_id": "$x", "f": {"$first": "$y"}}},
        {"$match": {"_id": {"$gt": 1}}},
    ],
    "on_a_document_key": [
        {"$group": {"_id": {"k": "$x"}, "f": {"$first": "$y"}}},
        {"$match": {"_id.k": {"$gt": 1}}},
    ],
    "on_an_accumulator": [
        {"$group": {"_id": "$x", "f": {"$first": "$y"}}},
        {"$match": {"f": {"$gt": 1}}},
    ],
}


@pytest.mark.backlog("RM-311")
def test_explain_moves_a_match_on_the_group_key_ahead_of_the_group(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 moves a `$match` on a `$group`'s `_id` ahead of the
    group, onto the field the key is, so the query reads it; one on an
    accumulator stays behind."""

    def read(client: Any) -> dict:
        planners = _distinct_group_planners(client, _MATCH_AFTER_GROUP)
        return {name: (p[0], p[1], p[3]) for name, p in planners.items()}

    got, want = read(rust_mongo), read(mongod)
    assert want["on_the_key"][0] == {"x": {"$gt": 1}}
    assert want["on_the_key"][2] == ["$groupByDistinctScan"]
    assert got == want


_STRENGTH_2 = {"locale": "en", "strength": 2}


def _read_before_emptied(client: Any, shape: str) -> int:
    """How many documents a cursor returns, a document at a time, when its
    collection is emptied after the first batch: what it had read by then."""
    db = client["rm_found_1025"]
    options = {"collation": _STRENGTH_2} if shape.startswith("default collation") else {}
    c = db["c" + uuid.uuid4().hex[:10]]
    db.create_collection(c.name, **options)
    c.insert_many(
        [{"_id": i, "x": i % 7, "s": ("A" if i % 2 else "a") + str(i % 5)} for i in range(1, 201)]
    )
    view = "v" + uuid.uuid4().hex[:8]
    command: dict
    if shape == "find on a view":
        db.command("create", view, viewOn=c.name, pipeline=[{"$match": {"x": {"$gte": 0}}}])
        command = {"find": view, "filter": {}, "batchSize": 1}
    elif shape == "aggregate on a view":
        db.command("create", view, viewOn=c.name, pipeline=[])
        command = {"aggregate": view, "pipeline": [], "cursor": {"batchSize": 1}}
    elif shape == "a view's $addFields":
        db.command("create", view, viewOn=c.name, pipeline=[{"$addFields": {"q": 1}}])
        command = {"aggregate": view, "pipeline": [], "cursor": {"batchSize": 1}}
    elif shape == "a collated view":
        db.command(
            "create", view, viewOn=c.name, pipeline=[{"$match": {"s": "a1"}}], collation=_STRENGTH_2
        )
        command = {"aggregate": view, "pipeline": [], "cursor": {"batchSize": 1}}
    elif shape == "find with a collation":
        command = {"find": c.name, "filter": {"s": "a1"}, "batchSize": 1, "collation": _STRENGTH_2}
    elif shape == "aggregate with a collation":
        command = {
            "aggregate": c.name,
            "pipeline": [{"$addFields": {"q": {"$eq": ["$s", "A1"]}}}],
            "cursor": {"batchSize": 1},
            "collation": _STRENGTH_2,
        }
    elif shape == "default collation, an index on a number":
        c.create_index("x")
        command = {
            "aggregate": c.name,
            "pipeline": [{"$match": {"x": {"$gte": 0}}}],
            "cursor": {"batchSize": 1},
        }
    else:  # a collated index on a string
        c.create_index("s", collation=_STRENGTH_2)
        command = {
            "aggregate": c.name,
            "pipeline": [{"$match": {"s": "a1"}}],
            "cursor": {"batchSize": 1},
            "collation": _STRENGTH_2,
        }
    name = command.get("find") or command.get("aggregate")
    reply = db.command(command)
    got, cursor = len(reply["cursor"]["firstBatch"]), reply["cursor"]["id"]
    c.delete_many({})
    while cursor:
        reply = db.command("getMore", cursor, collection=name, batchSize=1000)
        got += len(reply["cursor"]["nextBatch"])
        cursor = reply["cursor"]["id"]
    return got


_STREAMED_SHAPES = (
    "find on a view",
    "aggregate on a view",
    "a view's $addFields",
    "a collated view",
    "find with a collation",
    "aggregate with a collation",
    "default collation, an index on a number",
)


@pytest.mark.backlog_fixed("RM-080")
def test_a_cursor_through_a_view_or_under_a_collation_reads_on(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads through a view, and under a collation, as it reads
    a collection: batch by batch where its planner takes the read whole, 32
    documents first through the cursor stage."""
    got = {s: _read_before_emptied(rust_mongo, s) for s in _STREAMED_SHAPES}
    want = {s: _read_before_emptied(mongod, s) for s in _STREAMED_SHAPES}
    assert want["aggregate on a view"] == 1
    assert want["a view's $addFields"] == 32
    assert got == want


@pytest.mark.backlog("RM-080")
def test_a_cursor_over_a_collated_index_of_strings_reads_on(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads a collated index's strings batch by batch, as any
    index; this store's collated keys are not in mongod's order, and the
    cursor reads every document at the command."""
    shape = "a collated index on a string"
    assert _read_before_emptied(mongod, shape) == 1
    assert _read_before_emptied(rust_mongo, shape) == _read_before_emptied(mongod, shape)


def _dropped_under(client: Any, through_view: bool, pipeline: list, drop: str) -> tuple:
    """A cursor read five at a time after its collection, one of its
    indexes, or the view it reads through is dropped: how many documents it
    returned, and how it ended."""
    db = client["rm_found_1025"]
    c = _coll(client)
    c.insert_many([{"_id": i, "x": i} for i in range(1, 201)])
    c.create_index("x")
    name = c.name
    if through_view:
        name = "v" + uuid.uuid4().hex[:8]
        db.command("create", name, viewOn=c.name, pipeline=[])
    reply = db.command("aggregate", name, pipeline=pipeline, cursor={"batchSize": 1})
    if drop == "collection":
        c.drop()
    elif drop == "index":
        c.drop_index("x_1")
    else:
        db.drop_collection(name)
    got, cursor = 1, reply["cursor"]["id"]
    while cursor:
        more = mongo_answer(
            lambda cursor=cursor: db.command("getMore", cursor, collection=name, batchSize=5)
        )
        if more[0] == "err":
            return got, more[1]
        got += len(more[1]["cursor"]["nextBatch"])
        cursor = more[1]["cursor"]["id"]
    return got, "end"


@pytest.mark.backlog_fixed("RM-080")
def test_a_cursor_returns_what_it_buffered_when_its_collection_is_dropped(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 ends a cursor whose collection, or an index its plan
    reads, is dropped: a plan executor's read at the next `getMore`, one
    through the cursor stage at the first `getMore` that needs more than it
    buffered (175).
    Through a view it is the collection beneath that counts: dropping the
    view leaves the cursor reading."""
    match = [{"$match": {"x": {"$gte": 0}}}]
    buffered = [*match, {"$addFields": {"q": 1}}]
    shapes = {
        f"{where} {name} {drop}": (where == "view", pipeline, drop)
        for where in ("collection", "view")
        for name, pipeline in (("planned", match), ("buffered", buffered))
        for drop in ("collection", "index")
    }
    shapes["view planned view"] = (True, match, "view")
    got = {k: _dropped_under(rust_mongo, *v) for k, v in shapes.items()}
    want = {k: _dropped_under(mongod, *v) for k, v in shapes.items()}
    assert want["collection buffered collection"] == (31, 175)
    assert want["view planned index"] == (1, 175)
    assert want["view planned view"] == (200, "end")
    assert got == want


def _sorted_ties(client: Any) -> dict:
    """The order of documents a blocking sort leaves equal."""
    c = _coll(client)
    c.insert_many([{"_id": i, "s": ("A" if i % 2 else "a") + str(i % 5)} for i in range(1, 201)])
    return {
        "simple": [d["_id"] for d in c.find({}, sort=[("s", 1)])][:20],
        "collated": [d["_id"] for d in c.find({}, sort=[("s", 1)], collation=_STRENGTH_2)][:20],
    }


@pytest.mark.backlog("RM-312")
def test_a_blocking_sort_leaves_ties_as_mongods_sort_does(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sorts a blocking sort's documents with an unstable sort:
    of 200 documents in 10 runs of 20 equal keys, the first is not the first
    inserted, and two mongod processes agree. This server keeps equal
    documents in the order it reads them."""
    want = _sorted_ties(mongod)
    assert want["simple"][:2] != [5, 15]
    assert _sorted_ties(rust_mongo) == want


def _killed_while_running(client: Any) -> tuple:
    """A slow retryable update, and `killSessions` on its session while it
    runs: both replies' `ok` and `code`, and whether the update applied."""
    import threading
    import time

    from bson import Binary, Int64

    from tests.backlog.test_rm_found_2026_10_21 import _op_msg, _slow_update

    host, port = client.address
    lsid = {"id": Binary(uuid.uuid4().bytes, 4)}
    cmd = {
        "update": "c",
        "updates": [_slow_update(host, port, "rm313")],
        "lsid": lsid,
        "txnNumber": Int64(1),
        "$db": "rm313",
    }
    first: list = []
    thread = threading.Thread(target=lambda: first.append(_op_msg(host, port, cmd)), daemon=True)
    thread.start()
    # Kill it once it runs: on a loaded box 0.3 s may not see it started.
    running = {"aggregate": 1, "cursor": {}, "$db": "admin"}
    running["pipeline"] = [{"$currentOp": {}}, {"$match": {"ns": "rm313.c", "op": "update"}}]
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        time.sleep(0.3)
        if _op_msg(host, port, running)["cursor"]["firstBatch"]:
            break
    killed = _op_msg(host, port, {"killSessions": [lsid], "$db": "admin"})
    thread.join(timeout=120)
    assert first, "the update never answered"
    applied = _op_msg(host, port, {"find": "c", "$db": "rm313"})["cursor"]["firstBatch"][0]["n"]
    return (
        (first[0].get("ok"), first[0].get("code")),
        (killed.get("ok"), killed.get("code")),
        applied,
    )


@pytest.mark.backlog_fixed("RM-313")
def test_kill_sessions_interrupts_the_command_holding_the_session(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11: `killSessions` on a session whose retryable update is
    running interrupts that update (11601) and it does not apply; the kill
    answers ok. This server marked nothing on the session's operations, and
    read a kill only between the documents a scan reads, so the update ran
    to the end and applied."""
    theirs = _killed_while_running(mongod_rs)
    assert theirs == ((0.0, 11601), (1.0, None), 0), theirs  # the reference, as measured
    assert _killed_while_running(rust_mongo) == theirs


def _read_beside_an_open_transaction(client: Any) -> tuple:
    """A snapshot read at the current time while another session's
    transaction has written and not committed, with a one-second
    `maxTimeMS`; then the same read once it has committed."""
    db = client["rm314"]
    db.drop_collection("d")
    db.create_collection("d")
    db.d.insert_one({"_id": 0})
    with client.start_session() as s:
        s.start_transaction()
        db.d.insert_one({"_id": 1}, session=s)
        now = client.admin.command("ping")["operationTime"]
        cmd = {
            "find": "d",
            "readConcern": {"level": "snapshot", "atClusterTime": now},
            "maxTimeMS": 1000,
        }
        during = mongo_answer(lambda: [d["_id"] for d in db.command(cmd)["cursor"]["firstBatch"]])
        s.commit_transaction()
    after = mongo_answer(lambda: [d["_id"] for d in db.command(cmd)["cursor"]["firstBatch"]])
    return during[:2], after[:2]


@pytest.mark.backlog_fixed("RM-314")
def test_a_snapshot_read_does_not_wait_for_an_open_transaction(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 gives a transaction its timestamp when it commits, so a
    read at the current time beside an open one answers at once, without
    it, and the same afterwards. This server stamped a transaction with the
    time its first statement ran: the read waited for it, and once it
    committed the same read found it."""
    theirs = _read_beside_an_open_transaction(mongod_rs)
    assert theirs == (("ok", [0]), ("ok", [0])), theirs  # the reference, as measured
    assert _read_beside_an_open_transaction(rust_mongo) == theirs


def _transaction_in_the_oplog(client: Any) -> list:
    """The oplog's last three entries after a transaction whose two inserts
    straddle another client's write: `(op, ns, how many applyOps)`."""
    db = client["rm314o"]
    db.drop_collection("d")
    db.create_collection("d")
    with client.start_session() as s:
        s.start_transaction()
        db.d.insert_one({"_id": 1}, session=s)
        db.x.insert_one({"_id": 9})
        db.d.insert_one({"_id": 2}, session=s)
        s.commit_transaction()
    # Only this test's entries: a fresh mongod's first transaction also
    # creates its `config` collections.
    mine = {"ns": {"$in": ["rm314o.$cmd", "rm314o.x", "rm314o.d", "admin.$cmd"]}}
    tail = list(client.local.oplog.rs.find(mine).sort("$natural", -1).limit(3))[::-1]
    return [(e["op"], e["ns"], len(e.get("o", {}).get("applyOps", []))) for e in tail]


@pytest.mark.backlog_fixed("RM-314")
def test_a_transaction_is_one_entry_at_its_commit(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 logs a transaction as one `applyOps` at its commit,
    after a write another client made while it ran. This server logged each
    statement when it ran, so the other client's write came between them."""
    theirs = _transaction_in_the_oplog(mongod_rs)
    assert theirs == [
        ("c", "rm314o.$cmd", 0),
        ("i", "rm314o.x", 0),
        ("c", "admin.$cmd", 2),
    ], theirs  # the reference, as measured
    assert _transaction_in_the_oplog(rust_mongo) == theirs
