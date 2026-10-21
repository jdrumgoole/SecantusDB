"""Change streams against a one-member mongod 8.2 replica set: pre- and
post-images, `startAtOperationTime`, and `$changeStreamSplitLargeEvent`.

No CI test compared these with mongod (the coverage review of 2026-10-21):
the Rust tests check the server against itself. Each case makes the same
writes on both servers and compares the events, with the fields that are
per-server by nature (resume tokens, times, UUIDs, session ids) removed.
"""

from __future__ import annotations

import itertools
import re
import time
from collections.abc import Callable
from typing import Any

import pytest

from tests.mongod_live.conftest import same

_SEQ = itertools.count()


def _fresh(client: Any, name: str) -> Any:
    """A database of the same name on both servers (it is in every event's
    `ns`), dropped first so each run starts empty."""
    client.drop_database(name)
    return client[name]


def _no_times(answer: Any) -> Any:
    """An error's message names the event's clusterTime, which is per-server."""
    if isinstance(answer, tuple) and answer[0] == "err":
        return (*answer[:2], re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(T)", answer[2]))
    return answer


#: Fields that name this run rather than describe the change.
_PER_SERVER = {"_id", "clusterTime", "wallTime", "collectionUUID", "lsid", "txnNumber"}


#: Each case's `_changes` makes four writes, so four events.
_EXPECTED = 4


def _events(
    client: Any,
    setup: Callable[[Any], None],
    writes: Callable[[Any], None],
    db_name: str = "live_cs",
    **watch: Any,
) -> list:
    """The events for `writes`, read until all four arrive or 10 s pass (a
    missing event then shows as a shorter list, not a hang)."""
    db = _fresh(client, db_name)
    setup(db)
    out = []
    with db.c.watch(max_await_time_ms=200, **watch) as stream:
        writes(db)
        deadline = time.monotonic() + 10
        while len(out) < _EXPECTED and time.monotonic() < deadline:
            ev = stream.try_next()
            if ev is not None:
                out.append({k: v for k, v in ev.items() if k not in _PER_SERVER})
    return out


def _drain(stream: Any) -> list:
    """Every event the stream has, for a stream whose writes are done: read
    until three empty batches in a row follow at least one event, or 10 s."""
    out, idle = [], 0
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and not (out and idle >= 3):
        ev = stream.try_next()
        if ev is None:
            idle += 1
        else:
            out.append(ev)
            idle = 0
    return out


def _with_images(db: Any) -> None:
    db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])


def _plain(db: Any) -> None:
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])


def _changes(db: Any) -> None:
    db.c.update_one({"_id": 1}, {"$set": {"a": 10}, "$unset": {"zz": ""}})
    db.c.replace_one({"_id": 2}, {"b": 2})
    db.c.delete_one({"_id": 1})
    db.c.insert_one({"_id": 3, "a": 3})


CASES = [
    ("pre-images required", _with_images, {"full_document_before_change": "required"}),
    ("pre-images whenAvailable", _with_images, {"full_document_before_change": "whenAvailable"}),
    (
        "pre-images whenAvailable, not enabled",
        _plain,
        {"full_document_before_change": "whenAvailable"},
    ),
    ("post-images updateLookup", _plain, {"full_document": "updateLookup"}),
    # Post-images (`fullDocument: "whenAvailable"` / `"required"` on a
    # collection with changeStreamPreAndPostImages) diverge: RM-229.
    ("projection", _plain, {"pipeline": [{"$project": {"operationType": 1, "documentKey": 1}}]}),
]


@pytest.mark.parametrize(("name", "setup", "watch"), CASES, ids=[n for n, _, _ in CASES])
def test_change_events_match_mongod(
    name: str, setup: Callable, watch: dict, rust_mongo: Any, mongod_rs: Any
) -> None:
    same(
        _events(rust_mongo, setup, _changes, **watch), _events(mongod_rs, setup, _changes, **watch)
    )


def test_required_pre_image_that_is_not_there(rust_mongo: Any, mongod_rs: Any) -> None:
    """`fullDocumentBeforeChange: "required"` on a collection without
    pre-images: mongod fails the stream (the error is the comparison)."""
    from tests.mongod_live.conftest import mongo_answer

    answers = []
    for client in (rust_mongo, mongod_rs):
        answers.append(
            mongo_answer(
                lambda c=client: _events(
                    c, _plain, _changes, "live_cs_req", full_document_before_change="required"
                )
            )
        )
    same(*[_no_times(a) for a in answers])


def _from_operation_time(client: Any) -> list:
    db = _fresh(client, "live_cs_at")
    db.c.insert_one({"_id": "before"})
    with client.start_session() as s:
        db.c.insert_one({"_id": "at"}, session=s)
        t = s.operation_time
    db.c.insert_one({"_id": "after"})
    with db.c.watch(start_at_operation_time=t, max_await_time_ms=200) as stream:
        return [(ev["operationType"], ev["documentKey"]) for ev in _drain(stream)]


def test_start_at_operation_time_matches_mongod(rust_mongo: Any, mongod_rs: Any) -> None:
    """A stream started at a write's operationTime sees that write and every
    later one, and nothing earlier."""
    same(_from_operation_time(rust_mongo), _from_operation_time(mongod_rs))


def _split(client: Any) -> list:
    db = _fresh(client, "live_cs_split")
    _with_images(db)
    big = "x" * (9 * 1024 * 1024)
    pipeline = [{"$changeStreamSplitLargeEvent": {}}]
    # Pre-images only (post-images are RM-229): the second update's event
    # carries a 9 MB pre-image AND a 9 MB updateDescription.
    with db.c.watch(
        pipeline, full_document_before_change="required", max_await_time_ms=200
    ) as stream:
        db.c.update_one({"_id": 1}, {"$set": {"big": big}})
        db.c.update_one({"_id": 1}, {"$set": {"big2": big}, "$unset": {"big": ""}})
        return [
            {"splitEvent": ev.get("splitEvent"), "keys": [k for k in ev if k not in _PER_SERVER]}
            for ev in _drain(stream)
        ]


def test_split_large_event_matches_mongod(rust_mongo: Any, mongod_rs: Any) -> None:
    """An event over 16 MB arrives as fragments: which fields each fragment
    carries, and its `splitEvent` numbering."""
    theirs = _split(mongod_rs)
    # Not vacuous: the reference did split the second event.
    assert any(e["splitEvent"] for e in theirs), theirs
    same(_split(rust_mongo), theirs)
