"""End-to-end change-stream tests over the wire via ``pymongo``.

Each test gets its own ``SecantusDBServer`` on a random ephemeral port and
its own in-memory storage — no shared state, safe under ``pytest -n auto``.

A small ``_drain`` helper polls the change stream from a background thread
so the main test thread can drive writes against the same server. We deal
in real wall-clock timeouts; the producer wakes within ~1s of any write.
"""

from __future__ import annotations

import threading
import time
from collections.abc import Iterator
from typing import Any

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path) -> Iterator[SecantusDBServer]:
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer) -> Iterator[MongoClient]:
    mc = MongoClient(server.uri, directConnection=True, serverSelectionTimeoutMS=5000)
    try:
        yield mc
    finally:
        mc.close()


def _open(cs):
    """Force a lazily-created change stream to actually open, and return it.

    pymongo's ``ChangeStream.__init__`` does NOT contact the server: the
    ``aggregate`` that fixes the stream's start position runs on the FIRST
    iteration. So ``coll.watch(...)`` followed by ``time.sleep(...)``
    establishes nothing — the start position is captured whenever something
    first pulls on the cursor, which may be after writes the test intends the
    stream to observe.

    When that happens the miss is PERMANENT, not slow: the server sets
    ``start_seq = oplog_tail_seq() + 1`` at aggregate time, so an earlier write
    is excluded from the stream forever and no amount of polling surfaces it.
    That is what CI showed — "awaitData did not surface the insert within 30s
    (insert completed 29.9s ago)", i.e. ~30 poll cycles against an event the
    stream had already been told to skip.

    ``try_next()`` drives the aggregate and returns None when nothing is
    pending, which is exactly the "stream is now open and positioned" barrier
    these tests assumed ``watch()`` gave them.
    """
    assert cs.try_next() is None, "stream had events before the test wrote any"
    return cs


def _drain(
    cs,
    target: int,
    timeout: float = 8.0,
    arrivals: list[float] | None = None,
) -> list[dict[str, Any]]:
    """Collect ``target`` events, or give up after ``timeout``.

    ``arrivals``, if given, receives the monotonic timestamp at which each
    event was observed. Callers timing a wake need that instant, not the time
    this helper returns: the poll loop below only notices a new event within
    its 0.05s tick and then joins the runner for up to 0.5s, which is noise on
    the scale of a sub-second wake bound.
    """
    events: list[dict[str, Any]] = []
    deadline = time.monotonic() + timeout
    err: list[BaseException] = []

    def runner() -> None:
        try:
            for ev in cs:
                events.append(ev)
                if arrivals is not None:
                    arrivals.append(time.monotonic())
                if len(events) >= target:
                    return
        except BaseException as exc:
            err.append(exc)

    t = threading.Thread(target=runner, daemon=True)
    t.start()
    while time.monotonic() < deadline:
        if len(events) >= target or err:
            break
        time.sleep(0.05)
    t.join(timeout=0.5)
    if err:
        raise err[0]
    return events


def test_collection_watch_insert_update_delete(client: MongoClient) -> None:
    db = client["csdb1"]
    coll = db["c"]
    db.create_collection("c")

    cs = coll.watch(max_await_time_ms=2000)
    time.sleep(0.3)  # let cursor establish before driving writes

    coll.insert_one({"_id": 1, "x": 1})
    coll.update_one({"_id": 1}, {"$set": {"x": 99}})
    coll.delete_one({"_id": 1})

    events = _drain(cs, target=3)
    cs.close()
    assert [e["operationType"] for e in events] == ["insert", "update", "delete"]
    for e in events:
        assert e["documentKey"] == {"_id": 1}
        assert e["ns"] == {"db": "csdb1", "coll": "c"}
    assert events[0]["fullDocument"] == {"_id": 1, "x": 1}
    assert events[1]["updateDescription"]["updatedFields"] == {"x": 99}


def test_db_watch_sees_all_collections(client: MongoClient) -> None:
    db = client["csdb_dbwatch"]
    db.create_collection("a")
    db.create_collection("b")

    cs = _open(db.watch(max_await_time_ms=2000))

    db["a"].insert_one({"_id": 1})
    db["b"].insert_one({"_id": 2})

    events = _drain(cs, target=2)
    cs.close()
    seen = {(e["ns"]["coll"], e["documentKey"]["_id"]) for e in events}
    assert seen == {("a", 1), ("b", 2)}


def test_cluster_watch_sees_all_databases(client: MongoClient) -> None:
    client["csdb_cluster_a"].create_collection("c")
    client["csdb_cluster_b"].create_collection("c")
    cs = _open(client.watch(max_await_time_ms=2000))

    client["csdb_cluster_a"]["c"].insert_one({"_id": 1})
    client["csdb_cluster_b"]["c"].insert_one({"_id": 2})

    events = _drain(cs, target=2)
    cs.close()
    seen = {(e["ns"]["db"], e["documentKey"]["_id"]) for e in events}
    assert seen == {("csdb_cluster_a", 1), ("csdb_cluster_b", 2)}


def test_resume_after_token_picks_up_subsequent_events(client: MongoClient) -> None:
    db = client["csdb_resume"]
    coll = db["c"]
    db.create_collection("c")

    cs1 = _open(coll.watch(max_await_time_ms=2000))
    coll.insert_one({"_id": 1})
    coll.insert_one({"_id": 2})
    events1 = _drain(cs1, target=2)
    cs1.close()
    token = events1[-1]["_id"]
    assert "_data" in token

    coll.insert_one({"_id": 3})
    coll.insert_one({"_id": 4})
    cs2 = coll.watch(resume_after=token, max_await_time_ms=2000)
    events2 = _drain(cs2, target=2)
    cs2.close()
    keys = [e["documentKey"]["_id"] for e in events2]
    assert keys == [3, 4]


@pytest.mark.parametrize("resume_option", ["resume_after", "start_after"])
def test_resumed_open_returns_backlog_in_first_batch(
    client: MongoClient, resume_option: str
) -> None:
    """A resumed open (resumeAfter / startAfter) must return the already-
    committed backlog events in the aggregate's firstBatch, so a driver that
    checks the cursor for buffered data *before* issuing any getMore sees
    them — pymongo's ``CommandCursor._has_next()`` never sends a getMore.
    And because firstBatch is non-empty, pymongo doesn't overwrite the
    cached resume token from the open response's postBatchResumeToken, so an
    uniterated resumed stream still reports ``resume_token == <the token
    passed in>`` (pymongo change-streams prose test #14)."""
    db = client["csdb_backlog"]
    coll = db["c"]
    db.create_collection("c")

    cs1 = _open(coll.watch(max_await_time_ms=2000))
    coll.insert_one({"_id": 0})
    resume_point = _drain(cs1, target=1)[-1]["_id"]
    cs1.close()

    # Commit a backlog the resumed stream must surface on open.
    coll.insert_many([{"_id": 1}, {"_id": 2}, {"_id": 3}])

    cs2 = coll.watch(**{resume_option: resume_point}, max_await_time_ms=2000)
    # firstBatch is populated: buffered data is visible without a getMore.
    assert cs2._cursor._has_next() is True
    # Uniterated: the cached resume token is still the one we passed in.
    assert cs2.resume_token == resume_point
    keys = [e["documentKey"]["_id"] for e in _drain(cs2, target=3)]
    cs2.close()
    assert keys == [1, 2, 3]


def test_fresh_watch_has_empty_first_batch(client: MongoClient) -> None:
    """A fresh (non-resuming) watch positions at the oplog tail: there is no
    backlog, so firstBatch stays empty and the cursor has no buffered data
    until a write arrives. Guards against the resumed-open backlog drain
    leaking into the common tail-watch path."""
    db = client["csdb_fresh"]
    coll = db["c"]
    db.create_collection("c")
    cs = coll.watch(max_await_time_ms=2000)
    assert cs._cursor._has_next() is False
    coll.insert_one({"_id": 1})
    keys = [e["documentKey"]["_id"] for e in _drain(cs, target=1)]
    cs.close()
    assert keys == [1]


def test_start_at_operation_time_picks_up_new_events(client: MongoClient) -> None:
    db = client["csdb_sat"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 1})
    op_time = client["csdb_sat"].command("hello")["lastWrite"]["opTime"]["ts"]
    coll.insert_one({"_id": 2})
    coll.insert_one({"_id": 3})
    cs = coll.watch(start_at_operation_time=op_time, max_await_time_ms=2000)
    events = _drain(cs, target=2)
    cs.close()
    keys = [e["documentKey"]["_id"] for e in events]
    assert keys == [2, 3]


def test_full_document_update_lookup_returns_post_state(client: MongoClient) -> None:
    db = client["csdb_lookup"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 1, "x": 0})
    cs = _open(coll.watch(full_document="updateLookup", max_await_time_ms=2000))
    coll.update_one({"_id": 1}, {"$set": {"x": 5}})
    events = _drain(cs, target=1)
    cs.close()
    assert events[0]["operationType"] == "update"
    assert events[0]["fullDocument"] == {"_id": 1, "x": 5}


def test_full_document_update_lookup_after_delete_is_none(client: MongoClient) -> None:
    db = client["csdb_lookup_gone"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 1, "x": 0})
    cs = _open(coll.watch(full_document="updateLookup", max_await_time_ms=2000))
    coll.update_one({"_id": 1}, {"$set": {"x": 5}})
    coll.delete_one({"_id": 1})
    events = _drain(cs, target=2)
    cs.close()
    update_ev = next(e for e in events if e["operationType"] == "update")
    assert update_ev["fullDocument"] is None or update_ev["fullDocument"] == {"_id": 1, "x": 5}


def test_full_document_before_change_when_enabled(client: MongoClient) -> None:
    db = client["csdb_pre"]
    coll = db["c"]
    db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
    coll.insert_one({"_id": 1, "x": 1})

    cs = coll.watch(
        full_document_before_change="whenAvailable",
        max_await_time_ms=2000,
    )
    time.sleep(0.3)
    coll.update_one({"_id": 1}, {"$set": {"x": 9}})
    coll.delete_one({"_id": 1})
    events = _drain(cs, target=2)
    cs.close()
    update_ev = next(e for e in events if e["operationType"] == "update")
    delete_ev = next(e for e in events if e["operationType"] == "delete")
    assert update_ev["fullDocumentBeforeChange"]["x"] == 1
    assert delete_ev["fullDocumentBeforeChange"]["x"] == 9


def test_invalidate_on_drop_collection(client: MongoClient) -> None:
    db = client["csdb_inv"]
    coll = db["c"]
    db.create_collection("c")
    cs = _open(coll.watch(max_await_time_ms=2000))
    coll.insert_one({"_id": 1})
    coll.drop()
    events = _drain(cs, target=3)
    cs.close()
    op_types = [e["operationType"] for e in events]
    # Order: insert, drop, invalidate
    assert "insert" in op_types
    assert "drop" in op_types
    assert "invalidate" in op_types
    # The invalidate should be the terminal event.
    assert op_types[-1] == "invalidate"


def test_invalidate_on_rename(client: MongoClient) -> None:
    db = client["csdb_rename"]
    db.create_collection("src")
    cs = db["src"].watch(max_await_time_ms=2000)
    time.sleep(0.3)
    db["src"].insert_one({"_id": 1})
    db["src"].rename("dst")
    events = _drain(cs, target=3)
    cs.close()
    op_types = [e["operationType"] for e in events]
    assert "rename" in op_types
    assert op_types[-1] == "invalidate"


def test_await_data_blocks_then_wakes_on_insert(client: MongoClient) -> None:
    """awaitData must block on an idle stream and wake when a write lands.

    Timing here is split in two ON PURPOSE, because conflating the halves is
    what made this test flake under CI load (seen on #512: 0 events instead of
    1, passing on rerun of the same commit).

    What this test pins is that a stream idling in awaitData still SURFACES a
    later write. Everything before the wake (starting a thread, its sleep, the
    insert round-trip) is setup, and on a loaded CI runner — four xdist workers
    sharing four vCPUs, each insert paying a journal + checkpoint fsync in the
    durable lane — that setup can take seconds. The previous version measured
    one budget from thread start and asserted both halves against it, so
    scheduling and fsync noise was charged against CORRECTNESS: the drain
    deadline could expire before the inserting thread had even run, and the
    failure read as "the event never arrived" rather than "the box was busy".

    The latency bound below is a loose sanity check, not the promptness proof:
    with max_await_time_ms=1000 pymongo re-polls, so an event surfaces within a
    window or two even if nothing woke early. ``test_await_data_write_between_
    drain_and_wait_wakes_promptly`` is the test that pins prompt-wake
    semantics, and it does so deterministically via a monkeypatched drain
    rather than by racing a clock.

    So: the drain gets a deliberately generous ceiling (it returns in ~1s when
    healthy, and only a genuine failure to wake spends it), while the latency
    bound is measured from the instant the insert actually COMPLETED — which
    the inserting thread records — to the instant the event arrived.
    """
    db = client["csdb_block"]
    coll = db["c"]
    db.create_collection("c")
    # Per-getMore await window well under the drain ceiling, so pymongo
    # re-polls several times rather than staking everything on one window.
    cs = _open(coll.watch(max_await_time_ms=1000))

    insert_done_at: list[float] = []
    insert_err: list[BaseException] = []

    def insert_after_delay() -> None:
        try:
            time.sleep(0.6)
            coll.insert_one({"_id": 1, "x": 1})
            insert_done_at.append(time.monotonic())
        except BaseException as exc:  # surfaced below, not swallowed
            insert_err.append(exc)

    inserter = threading.Thread(target=insert_after_delay, daemon=True)
    arrivals: list[float] = []
    inserter.start()
    events = _drain(cs, target=1, timeout=30.0, arrivals=arrivals)
    cs.close()
    inserter.join(timeout=10.0)

    if insert_err:
        raise AssertionError(f"the inserting thread failed: {insert_err[0]!r}")
    # Distinguish "the write never happened" from "the stream never woke" —
    # the old assertion reported both as `0 == 1`.
    assert insert_done_at, "the inserting thread never completed its write"
    assert len(events) == 1, (
        f"awaitData did not surface the insert within 30s "
        f"(insert completed {time.monotonic() - insert_done_at[0]:.1f}s ago)"
    )

    wake_latency = arrivals[0] - insert_done_at[0]
    # Worst case the write lands just after a poll opened, so one full
    # max_await_time_ms (1s) plus a round trip; 5s leaves generous headroom for
    # a loaded runner while still failing a stream that slept its whole window.
    assert wake_latency < 5.0, f"woke {wake_latency:.1f}s after the insert"


def test_await_data_write_between_drain_and_wait_wakes_promptly(
    server: SecantusDBServer, client: MongoClient, monkeypatch
) -> None:
    """Regression: a write landing between the getMore's producer drain and
    its awaitData wait must trip the wake predicate. The buggy ordering
    captured the oplog tail *after* the drain, so such a write was counted
    into the captured tail and the getMore slept its full maxTimeMS with the
    event already in the oplog (surfacing it only on the post-wait re-drain).
    The monkeypatched drain lands an insert immediately after the first
    getMore drain returns empty — deterministically inside the race window.
    """
    from secantus import commands as commands_mod

    db = client["csdb_gap"]
    coll = db["c"]
    db.create_collection("c")

    real_drain = commands_mod._drain_change_stream_producer
    fired = threading.Event()

    def drain_then_insert(entry: Any) -> None:
        real_drain(entry)
        if not fired.is_set() and not entry.remaining:
            fired.set()
            server.storage.insert("csdb_gap", "c", [{"_id": 1, "x": 1}])

    monkeypatch.setattr(commands_mod, "_drain_change_stream_producer", drain_then_insert)

    max_await_ms = 2000
    cs = coll.watch(max_await_time_ms=max_await_ms)
    start = time.monotonic()
    events = _drain(cs, target=1, timeout=6.0)
    elapsed = time.monotonic() - start
    cs.close()
    assert fired.is_set()
    assert len(events) == 1
    assert events[0]["operationType"] == "insert"
    # The wake must be immediate (predicate already true), not the full
    # maxTimeMS sleep the buggy capture-after-drain ordering produced.
    assert elapsed < max_await_ms / 1000.0 * 0.75


def test_resume_after_pruned_token_raises_history_lost(server, client) -> None:
    db = client["csdb_pruned"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 1})
    cs = _open(coll.watch(max_await_time_ms=1000))
    coll.insert_one({"_id": 2})
    events = _drain(cs, target=1)
    cs.close()
    token = events[-1]["_id"]
    # Force-prune the entire oplog.
    server.storage.oplog_retention_seconds = 0.0
    server.storage.prune_oplog(now=10_000_000_000.0)
    # Resuming now raises ChangeStreamHistoryLost (286), wrapped by pymongo as OperationFailure.
    with pytest.raises(OperationFailure) as exc_info:
        coll.watch(resume_after=token, max_await_time_ms=500)
    assert exc_info.value.code == 286


def test_change_stream_with_post_match_filters_events(client: MongoClient) -> None:
    db = client["csdb_postmatch"]
    coll = db["c"]
    db.create_collection("c")
    cs = coll.watch(
        pipeline=[{"$match": {"operationType": "insert"}}],
        max_await_time_ms=2000,
    )
    time.sleep(0.3)
    coll.insert_one({"_id": 1})
    coll.update_one({"_id": 1}, {"$set": {"x": 1}})
    coll.insert_one({"_id": 2})
    events = _drain(cs, target=2)
    cs.close()
    assert all(e["operationType"] == "insert" for e in events)
    assert {e["documentKey"]["_id"] for e in events} == {1, 2}


def test_replace_emits_replace_operation_type(client: MongoClient) -> None:
    db = client["csdb_replace"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 7, "x": 1})

    cs = _open(coll.watch(max_await_time_ms=2000))

    coll.replace_one({"_id": 7}, {"_id": 7, "x": 99, "y": "added"})

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    e = events[0]
    assert e["operationType"] == "replace"
    assert e["documentKey"] == {"_id": 7}
    assert e["fullDocument"] == {"_id": 7, "x": 99, "y": "added"}
    assert "updateDescription" not in e


def test_replace_with_match_filter_on_replace_operation_type(client: MongoClient) -> None:
    db = client["csdb_replace_match"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 1, "x": 1})

    cs = coll.watch(
        [{"$match": {"operationType": "replace"}}],
        full_document="updateLookup",
        max_await_time_ms=2000,
    )
    time.sleep(0.3)

    coll.update_one({"_id": 1}, {"$set": {"x": 2}})  # operator update — should be filtered out
    coll.replace_one({"_id": 1}, {"_id": 1, "x": 3})  # replacement — should pass

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    assert events[0]["operationType"] == "replace"
    assert events[0]["fullDocument"] == {"_id": 1, "x": 3}


def test_cursor_id_is_int64_random(client: MongoClient) -> None:
    db = client["csdb_cid"]
    db.create_collection("c")
    result = db.command(
        {
            "aggregate": "c",
            "pipeline": [{"$changeStream": {}}],
            "cursor": {},
        }
    )
    cid = result["cursor"]["id"]
    assert int(cid) > 2**32


def test_create_indexes_emits_change_event(client: MongoClient) -> None:
    """A `createIndexes` on a watched collection produces an event with
    operationType=createIndexes and the new index spec under
    operationDescription.indexes[0] WHEN show_expanded_events is set.
    Without the opt-in, mongod (and SecantusDB since v0.5.1b18)
    suppresses these so the v1 spec's event set stays stable."""
    db = client["csdb_create_idx"]
    coll = db["c"]
    db.create_collection("c")

    cs = _open(coll.watch(max_await_time_ms=2000, show_expanded_events=True))
    coll.create_index([("x", 1)])

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    e = events[0]
    assert e["operationType"] == "createIndexes"
    assert e["ns"] == {"db": "csdb_create_idx", "coll": "c"}
    indexes = e["operationDescription"]["indexes"]
    assert len(indexes) == 1
    assert indexes[0]["name"] == "x_1"
    assert indexes[0]["key"] == {"x": 1}


def test_drop_indexes_emits_change_event(client: MongoClient) -> None:
    """`dropIndexes` surfaces with operationType=dropIndexes and the
    index name under operationDescription.indexes[0] WHEN
    show_expanded_events is set."""
    db = client["csdb_drop_idx"]
    coll = db["c"]
    db.create_collection("c")
    coll.create_index([("x", 1)])  # one index to drop

    cs = _open(coll.watch(max_await_time_ms=2000, show_expanded_events=True))
    coll.drop_index("x_1")

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    e = events[0]
    assert e["operationType"] == "dropIndexes"
    assert e["ns"] == {"db": "csdb_drop_idx", "coll": "c"}
    # Full index description since the showExpandedEvents-shape fix (mongod
    # 7.0.12-probed): {v, key, name}, not just the name.
    assert e["operationDescription"]["indexes"] == [{"v": 2, "key": {"x": 1}, "name": "x_1"}]


def test_create_collection_emits_change_event(client: MongoClient) -> None:
    """A `create` (createCollection) surfaces with operationType=create
    on a db-scoped stream WHEN show_expanded_events is set."""
    db = client["csdb_create_coll"]
    db.create_collection("seed")  # ensure the db exists before watching
    cs = _open(db.watch(max_await_time_ms=2000, show_expanded_events=True))
    db.create_collection("foo")

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    e = events[0]
    assert e["operationType"] == "create"
    assert e["ns"] == {"db": "csdb_create_coll", "coll": "foo"}


def test_collmod_emits_modify_change_event(client: MongoClient) -> None:
    """A `collMod` surfaces with operationType=modify WHEN
    show_expanded_events is set; suppressed otherwise."""
    db = client["csdb_collmod"]
    coll = db["c"]
    db.create_collection("c")

    cs = _open(coll.watch(max_await_time_ms=2000, show_expanded_events=True))
    db.command({"collMod": "c"})

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    assert events[0]["operationType"] == "modify"
    assert events[0]["ns"] == {"db": "csdb_collmod", "coll": "c"}


def test_rename_event_has_operation_description_and_collection_uuid(
    client: MongoClient,
) -> None:
    """With show_expanded_events, a rename that drops an existing target
    carries operationDescription.{to,dropTarget}, and CRUD events on the
    watched collection carry collectionUUID (mongod 6.0+ expanded fields)."""
    db = client["csdb_rename_expand"]
    coll = db["c"]
    db.create_collection("c")
    db.create_collection("dst")  # rename target to drop

    cs = _open(coll.watch(max_await_time_ms=2000, show_expanded_events=True))
    coll.insert_one({"a": 1})
    coll.rename("dst", dropTarget=True)

    events = _drain(cs, target=2)
    cs.close()
    insert_ev, rename_ev = events[0], events[1]
    assert insert_ev["operationType"] == "insert"
    assert "collectionUUID" in insert_ev
    assert rename_ev["operationType"] == "rename"
    assert rename_ev["to"] == {"db": "csdb_rename_expand", "coll": "dst"}
    op_desc = rename_ev["operationDescription"]
    assert op_desc["to"] == {"db": "csdb_rename_expand", "coll": "dst"}
    assert "dropTarget" in op_desc


def test_create_and_modify_suppressed_without_show_expanded_events(
    client: MongoClient,
) -> None:
    """Default watch (no showExpandedEvents) suppresses create / modify
    DDL events — only the stable v1 event set surfaces."""
    db = client["csdb_no_expand_ddl"]
    db.create_collection("seed")
    cs = _open(db.watch(max_await_time_ms=1000))
    db.create_collection("foo")  # would be a create event if expanded
    db.command({"collMod": "foo"})  # would be a modify event if expanded
    db["seed"].insert_one({"_id": 1})  # the only event a default stream sees

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    assert events[0]["operationType"] == "insert"


def test_create_indexes_suppressed_without_show_expanded_events(client: MongoClient) -> None:
    """Default ``coll.watch()`` (no ``showExpandedEvents``) suppresses
    DDL "expanded" events like createIndexes / dropIndexes — matches
    mongod's default behaviour. The v1 spec's stable event set
    (insert / update / delete / replace / drop / dropDatabase /
    rename / invalidate) stays the surface unless the user opts in.
    """
    db = client["csdb_no_expand"]
    coll = db["c"]
    db.create_collection("c")

    cs = _open(coll.watch(max_await_time_ms=1000))
    coll.create_index([("x", 1)])
    # Insert one real event so _drain has something to find — otherwise
    # the await blocks for the full timeout.
    coll.insert_one({"_id": 1})

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    assert events[0]["operationType"] == "insert"


def test_index_lifecycle_at_database_scope(client: MongoClient) -> None:
    """db.watch() picks up createIndexes/dropIndexes from any collection
    in the database — same routing as the existing drop / dropDatabase
    DDL events."""
    db = client["csdb_idx_db_scope"]
    db.create_collection("a")
    db.create_collection("b")

    cs = _open(db.watch(max_await_time_ms=2000, show_expanded_events=True))
    db["a"].create_index([("x", 1)])
    db["b"].create_index([("y", 1)])

    events = _drain(cs, target=2)
    cs.close()
    op_types = {e["operationType"] for e in events}
    assert op_types == {"createIndexes"}
    coll_names = {e["ns"]["coll"] for e in events}
    assert coll_names == {"a", "b"}


def test_split_large_change_stream_events_attaches_envelope(client: MongoClient) -> None:
    """When the user opts into ``splitLargeChangeStreamEvents: True``,
    every event carries a ``splitEvent: {fragment, of}`` envelope.
    SecantusDB's events are never large enough to actually split, so
    every fragment is single (``{fragment: 1, of: 1}``) — drivers
    reassemble identically.

    The pymongo Collection.watch() API doesn't expose this option as a
    top-level kwarg in 4.x, so we drive the aggregate command directly
    with the ``$changeStream`` stage spec."""
    db = client["csdb_split"]
    coll = db["c"]
    db.create_collection("c")

    pipeline = [{"$changeStream": {"splitLargeChangeStreamEvents": True}}]
    cs = coll.aggregate(pipeline, batchSize=1, maxAwaitTimeMS=2000)
    time.sleep(0.3)
    coll.insert_one({"_id": 1, "x": 1})
    coll.update_one({"_id": 1}, {"$set": {"x": 2}})
    coll.delete_one({"_id": 1})

    events = _drain(cs, target=3)
    cs.close()
    assert [e["operationType"] for e in events] == ["insert", "update", "delete"]
    for e in events:
        assert e["splitEvent"] == {"fragment": 1, "of": 1}


def test_split_large_change_stream_events_omitted_by_default(client: MongoClient) -> None:
    """Without the option, events do *not* carry a ``splitEvent`` field —
    the envelope is only present when the user opts in."""
    db = client["csdb_split_off"]
    coll = db["c"]
    db.create_collection("c")

    cs = _open(coll.watch(max_await_time_ms=2000))
    coll.insert_one({"_id": 1})

    events = _drain(cs, target=1)
    cs.close()
    assert "splitEvent" not in events[0]


def test_pipeline_update_is_update_event_with_truncated_arrays(client: MongoClient) -> None:
    """An aggregation-pipeline update ([{$set: ...}]) is an "update" event
    with a computed updateDescription — never "replace". Regression: the
    replacement classifier iterated the pipeline LIST (whose elements are
    stage dicts, not $-prefixed keys) and emitted a full-doc oplog entry,
    so pymongo's "Test array truncation" unified spec saw "replace".
    Mirrors that spec's shape: shrinking an array surfaces only in
    updateDescription.truncatedArrays."""
    db = client["csdb_pipeline_update"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 1, "a": 1, "array": ["foo", {"a": "bar"}, 1, 2, 3]})
    cs = _open(coll.watch(max_await_time_ms=2000))
    coll.update_one({"_id": 1}, [{"$set": {"array": ["foo", {"a": "bar"}]}}])
    events = _drain(cs, target=1)
    cs.close()
    assert events[0]["operationType"] == "update"
    desc = events[0]["updateDescription"]
    assert desc["updatedFields"] == {}
    assert desc["removedFields"] == []
    assert desc["truncatedArrays"] == [{"field": "array", "newSize": 2}]


def test_resume_token_tracks_per_event_with_batch_size_one(client: MongoClient) -> None:
    """mongocxx spec prose #1 — "ChangeStream must continuously track the last
    seen resumeToken".

    With ``batchSize=1`` the resume token must advance after each single-event
    read, and once the stream is exhausted with no further activity it must stay
    equal to the last delivered event's token — not jump to a freshly-minted
    high-water-mark token. Regression for two coupled PBRT bugs: (1) the
    producer prefetches up to 200 oplog rows and reported the prefetch tail as
    the postBatchResumeToken regardless of ``batchSize`` (so all three reads saw
    the same token); (2) an empty getMore re-minted the token with a fresh
    cluster time even when the oplog tail had not moved (so the post-exhaustion
    token differed from the last event's)."""
    db = client["csrt"]
    coll = db["c"]
    cs = coll.watch(batch_size=1, max_await_time_ms=500)
    coll.insert_one({"a": 1})
    coll.insert_one({"b": 2})
    coll.insert_one({"c": 3})

    tokens: list[Any] = []
    deadline = time.time() + 10.0
    while len(tokens) < 3 and time.time() < deadline:
        if cs.try_next() is not None:
            tokens.append(cs.resume_token)
    assert len(tokens) == 3, f"only saw {len(tokens)} events: {tokens}"

    # Each single-event read advances the resume token.
    assert tokens[0] != tokens[1]
    assert tokens[1] != tokens[2]
    assert tokens[0] != tokens[2]

    # Exhausted with no further activity: the token stays at the last event's.
    assert cs.try_next() is None
    assert cs.resume_token == tokens[2]
    cs.close()


def test_expanded_events_match_mongod_shapes(client: MongoClient) -> None:
    """showExpandedEvents shapes, verbatim from a mongod 7.0.12 probe:
    createIndexes / dropIndexes events carry the full index description
    ({v, key, name} — dropIndexes included, via the key spec captured at drop
    time), and expanded update events always carry ``disambiguatedPaths``
    (an empty document when nothing was ambiguous) while unexpanded ones
    never do."""
    import time

    db = client["csx"]
    db.c.insert_one({"_id": 0})
    cs = db.c.watch(show_expanded_events=True, max_await_time_ms=300)
    db.c.create_index([("x", 1)])
    db.c.update_one({"_id": 0}, {"$set": {"a-c": 2}})
    db.c.drop_indexes()
    events = []
    deadline = time.time() + 15
    while time.time() < deadline and len(events) < 3:
        ev = cs.try_next()
        if ev is not None:
            events.append(ev)
    cs.close()
    assert [e["operationType"] for e in events] == ["createIndexes", "update", "dropIndexes"]
    create_ev, update_ev, drop_ev = events
    assert create_ev["operationDescription"] == {
        "indexes": [{"v": 2, "key": {"x": 1}, "name": "x_1"}]
    }
    assert update_ev["updateDescription"]["disambiguatedPaths"] == {}
    assert drop_ev["operationDescription"] == {
        "indexes": [{"v": 2, "key": {"x": 1}, "name": "x_1"}]
    }

    # Without the flag: no DDL events, and no disambiguatedPaths key.
    cs = db.c.watch(max_await_time_ms=300)
    db.c.create_index([("y", 1)])
    db.c.update_one({"_id": 0}, {"$set": {"n": 1}})
    plain = None
    deadline = time.time() + 15
    while time.time() < deadline and plain is None:
        plain = cs.try_next()
    cs.close()
    assert plain["operationType"] == "update"
    assert "disambiguatedPaths" not in plain["updateDescription"]


def test_empty_poll_cannot_skip_a_write_that_lands_mid_scan(
    server: SecantusDBServer, client: MongoClient
) -> None:
    """A write that lands *after* the producer's oplog scan is still delivered.

    When a poll finds no events for the watched namespace, the cursor skips
    forward past the uninteresting activity so a quiet collection's resume token
    keeps moving. That skip may only pass entries the scan actually examined.
    Bounding it by the oplog *tail* instead loses writes two ways: a write that
    commits between the scan and the tail read is counted by the tail but was
    never seen, and the tail is the highest seq *minted*, which a writer bumps
    before committing its batch — so it can name an entry no reader can see yet.
    Either way the cursor steps over the event and no later poll returns it:
    data loss, not delay.

    The window is microseconds wide, so this drives it deterministically by
    committing the write from inside the producer's own storage calls. Both
    seams are hooked so the test pins the BEHAVIOUR rather than today's call
    shape: whichever the producer uses, the write lands after its scan.
    """
    db = client["race_db"]
    db.create_collection("c")
    stream = db["c"].watch(max_await_time_ms=200)

    storage = server.storage
    real_scan = storage.read_oplog_scan
    real_tail_seq = storage.oplog_tail_seq
    landed = threading.Event()

    def land_the_write() -> None:
        if landed.is_set():
            return
        landed.set()
        writer = MongoClient(server.uri, directConnection=True)
        try:
            writer["race_db"]["c"].insert_one({"_id": "written-mid-scan"})
        finally:
            writer.close()

    def scan_then_write(**kwargs: Any) -> Any:
        rows = real_scan(**kwargs)
        if not rows[0]:
            land_the_write()  # commits after the scan, before the skip
        return rows

    def write_then_tail_seq() -> int:
        land_the_write()  # the pre-fix shape read the tail here, after scanning
        return real_tail_seq()

    storage.read_oplog_scan = scan_then_write  # type: ignore[method-assign]
    storage.oplog_tail_seq = write_then_tail_seq  # type: ignore[method-assign]
    try:
        deadline = time.time() + 15
        event = None
        while event is None and time.time() < deadline:
            event = stream.try_next()
    finally:
        storage.read_oplog_scan = real_scan  # type: ignore[method-assign]
        storage.oplog_tail_seq = real_tail_seq  # type: ignore[method-assign]
        stream.close()

    assert landed.is_set(), "the test never exercised the window it exists for"
    assert event is not None, (
        "the write that landed after the scan was never delivered — the cursor "
        "skipped past it and no later poll can return it"
    )
    assert event["operationType"] == "insert"
    assert event["documentKey"]["_id"] == "written-mid-scan"
