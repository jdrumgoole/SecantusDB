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


def _drain(cs, target: int, timeout: float = 8.0) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    deadline = time.monotonic() + timeout
    err: list[BaseException] = []

    def runner() -> None:
        try:
            for ev in cs:
                events.append(ev)
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

    cs = db.watch(max_await_time_ms=2000)
    time.sleep(0.3)

    db["a"].insert_one({"_id": 1})
    db["b"].insert_one({"_id": 2})

    events = _drain(cs, target=2)
    cs.close()
    seen = {(e["ns"]["coll"], e["documentKey"]["_id"]) for e in events}
    assert seen == {("a", 1), ("b", 2)}


def test_cluster_watch_sees_all_databases(client: MongoClient) -> None:
    client["csdb_cluster_a"].create_collection("c")
    client["csdb_cluster_b"].create_collection("c")
    cs = client.watch(max_await_time_ms=2000)
    time.sleep(0.3)

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

    cs1 = coll.watch(max_await_time_ms=2000)
    time.sleep(0.3)
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

    cs1 = coll.watch(max_await_time_ms=2000)
    time.sleep(0.3)
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
    cs = coll.watch(full_document="updateLookup", max_await_time_ms=2000)
    time.sleep(0.3)
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
    cs = coll.watch(full_document="updateLookup", max_await_time_ms=2000)
    time.sleep(0.3)
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
    cs = coll.watch(max_await_time_ms=2000)
    time.sleep(0.3)
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
    db = client["csdb_block"]
    coll = db["c"]
    db.create_collection("c")
    cs = coll.watch(max_await_time_ms=5000)
    time.sleep(0.3)

    delay = 0.6

    def insert_after_delay() -> None:
        time.sleep(delay)
        coll.insert_one({"_id": 1, "x": 1})

    inserter = threading.Thread(target=insert_after_delay, daemon=True)
    start = time.monotonic()
    inserter.start()
    events = _drain(cs, target=1, timeout=5.0)
    elapsed = time.monotonic() - start
    cs.close()
    inserter.join(timeout=2.0)
    assert len(events) == 1
    # Should wake within ~2 * delay of the insert; rough bound.
    assert elapsed < delay + 2.5


def test_resume_after_pruned_token_raises_history_lost(server, client) -> None:
    db = client["csdb_pruned"]
    coll = db["c"]
    db.create_collection("c")
    coll.insert_one({"_id": 1})
    cs = coll.watch(max_await_time_ms=1000)
    time.sleep(0.3)
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

    cs = coll.watch(max_await_time_ms=2000)
    time.sleep(0.3)

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

    cs = coll.watch(max_await_time_ms=2000, show_expanded_events=True)
    time.sleep(0.3)
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

    cs = coll.watch(max_await_time_ms=2000, show_expanded_events=True)
    time.sleep(0.3)
    coll.drop_index("x_1")

    events = _drain(cs, target=1)
    cs.close()
    assert len(events) == 1
    e = events[0]
    assert e["operationType"] == "dropIndexes"
    assert e["ns"] == {"db": "csdb_drop_idx", "coll": "c"}
    assert e["operationDescription"]["indexes"] == [{"name": "x_1"}]


def test_create_collection_emits_change_event(client: MongoClient) -> None:
    """A `create` (createCollection) surfaces with operationType=create
    on a db-scoped stream WHEN show_expanded_events is set."""
    db = client["csdb_create_coll"]
    db.create_collection("seed")  # ensure the db exists before watching
    cs = db.watch(max_await_time_ms=2000, show_expanded_events=True)
    time.sleep(0.3)
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

    cs = coll.watch(max_await_time_ms=2000, show_expanded_events=True)
    time.sleep(0.3)
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

    cs = coll.watch(max_await_time_ms=2000, show_expanded_events=True)
    time.sleep(0.3)
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
    cs = db.watch(max_await_time_ms=1000)
    time.sleep(0.3)
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

    cs = coll.watch(max_await_time_ms=1000)
    time.sleep(0.3)
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

    cs = db.watch(max_await_time_ms=2000, show_expanded_events=True)
    time.sleep(0.3)
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

    cs = coll.watch(max_await_time_ms=2000)
    time.sleep(0.3)
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
    cs = coll.watch(max_await_time_ms=2000)
    time.sleep(0.3)
    coll.update_one({"_id": 1}, [{"$set": {"array": ["foo", {"a": "bar"}]}}])
    events = _drain(cs, target=1)
    cs.close()
    assert events[0]["operationType"] == "update"
    desc = events[0]["updateDescription"]
    assert desc["updatedFields"] == {}
    assert desc["removedFields"] == []
    assert desc["truncatedArrays"] == [{"field": "array", "newSize": 2}]
