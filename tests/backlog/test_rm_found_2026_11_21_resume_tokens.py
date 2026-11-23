"""Gap tests for what working the resume tokens (storage-model 4b-4) found in
the events of index commands, compared with mongod 8.2.11 as a one-member
replica set. A resume token holds the event's `operationDescription`, so a
description that differs is also a token that differs.
"""

from __future__ import annotations

import time
import uuid
from typing import Any

import pymongo
import pytest

#: Per-server by nature.
_PER_SERVER = {"_id", "clusterTime", "wallTime", "collectionUUID"}


def _events(stream: Any, want: int) -> list:
    out, end = [], time.monotonic() + 10
    while len(out) < want and time.monotonic() < end:
        ev = stream.try_next()
        if ev is not None:
            out.append(ev)
    return out


def _strip(value: Any) -> Any:
    if isinstance(value, dict):
        return [(k, _strip(v)) for k, v in value.items() if k != "uuid"]
    if isinstance(value, list):
        return [_strip(v) for v in value]
    return value


def _collmod_index(client: Any) -> list:
    db = client["rm_rt_collmod"]
    name = "c" + uuid.uuid4().hex[:8]
    coll = db.create_collection(name)
    coll.create_index("a", name="a_1")
    with db.watch(show_expanded_events=True, max_await_time_ms=200) as stream:
        db.command("collMod", name, index={"keyPattern": {"a": 1}, "hidden": True})
        events = _events(stream, 1)
    return [_strip({k: v for k, v in e.items() if k not in _PER_SERVER | {"ns"}}) for e in events]


@pytest.mark.backlog("RM-733")
def test_a_collmod_of_an_index_is_a_modify_event_with_the_index(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """`collMod` with `index` makes a `modify` event whose
    `operationDescription` is `{index: {name, hidden}}` and whose
    `stateBeforeChange` adds `indexOptions: {hidden: false}` to the
    collection's options (mongod 8.2.11). This server's event has an empty
    description and no `indexOptions`, and so a different resume token."""
    theirs = _collmod_index(mongod_rs)
    assert theirs
    assert dict(theirs[0])["operationDescription"] == [
        ("index", [("name", "a_1"), ("hidden", True)])
    ]
    assert _collmod_index(rust_mongo) == theirs


def _drop_all_indexes(client: Any) -> list:
    db = client["rm_rt_dropall"]
    name = "c" + uuid.uuid4().hex[:8]
    coll = db.create_collection(name)
    coll.create_indexes(
        [pymongo.IndexModel("a", unique=True), pymongo.IndexModel([("b", 1), ("c", -1)])]
    )
    coll.create_index("d", name="d_1", hidden=True)
    # A collMod rewrites the index's catalog entry: mongod then lists (and
    # drops) it last, with `hidden` after `name`.
    db.command("collMod", name, index={"name": "a_1", "hidden": True})
    with db.watch(show_expanded_events=True, max_await_time_ms=200) as stream:
        coll.drop_indexes()
        events = _events(stream, 3)
    return [
        e["operationDescription"]["indexes"][0]
        for e in events
        if e["operationType"] == "dropIndexes"
    ]


@pytest.mark.backlog("RM-734")
def test_dropping_every_index_lists_a_collmodded_index_last_with_hidden_after_name(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """`dropIndexes: "*"` logs one entry per index in catalog order: build
    order, except that an index a `collMod` changed moves to the end, its
    spec then `v`, the other options, `key`, `name`, `hidden` (mongod
    8.2.11; an index created hidden keeps `hidden` before `key`)."""
    theirs = _drop_all_indexes(mongod_rs)
    assert [s["name"] for s in theirs] == ["b_1_c_-1", "d_1", "a_1"]
    assert list(theirs[-1])[-1] == "hidden"
    ours = _drop_all_indexes(rust_mongo)
    assert [(s["name"], list(s)) for s in ours] == [(s["name"], list(s)) for s in theirs]
