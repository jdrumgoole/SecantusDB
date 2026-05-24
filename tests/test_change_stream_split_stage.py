"""``$changeStreamSplitLargeEvent`` accepted as a pass-through stage.

Drivers (mongo-rust-driver, mongo-node-driver, …) insert this
stage into the change-stream pipeline when the user opts into
``splitLargeChangeStreamEvents``. SecantusDB previously rejected
the stage at pipeline-parse time with ``unsupported aggregation
stage`` (code 14), which broke every change-stream usage with
that opt-in regardless of event size.

After the fix the stage is a no-op: docs pass through unchanged
in the pipeline. The split envelope itself
(``splitEvent: {fragment: 1, of: 1}``) is still applied upstream
by the change-stream producer when the user opts in via
``$changeStream: {showExpandedEvents: true, ...}``.

Real event splitting (``of`` > 1 for events that exceed 16 MB)
isn't implemented: events stay ``{fragment: 1, of: 1}``. So
mongo-rust-driver's ``test::change_stream::split_large_event``
test — which constructs a >16 MB update and asserts the second
fragment exists — still won't pass against SecantusDB. But it
no longer fails at the pipeline-parse step, and the typical
user path (opt-in + normal-size events) works cleanly.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


def test_split_stage_pipeline_parses(client) -> None:
    """``aggregate`` with
    ``[{$changeStream: {}}, {$changeStreamSplitLargeEvent: {}}]``
    is accepted at parse time. Previously the second stage was
    rejected with code 14 (TypeMismatch) ``unsupported aggregation
    stage``. After the fix the command succeeds, returns a
    tailable cursor id, and the cursor can be killed cleanly."""
    coll = client["css_db"]["pipeline_parses"]
    coll.insert_one({"_id": 1, "v": "seed"})

    resp = client["css_db"].command(
        "aggregate",
        "pipeline_parses",
        pipeline=[
            {"$changeStream": {}},
            {"$changeStreamSplitLargeEvent": {}},
        ],
        cursor={},
    )
    assert resp["ok"] == 1.0
    cursor_id = resp["cursor"]["id"]
    # firstBatch is empty (no events buffered yet) — the cursor
    # is tailable + awaitData.
    assert resp["cursor"]["firstBatch"] == []
    # Cleanly kill the cursor so the test doesn't leak it.
    if cursor_id != 0:
        client["css_db"].command("killCursors", "pipeline_parses", cursors=[cursor_id])


def test_split_stage_bad_spec_rejected_standalone(client) -> None:
    """A non-document spec (other than ``{}``) is rejected with
    ``AggregateError`` when the stage actually executes. With a
    leading ``$changeStream``, the command handler routes through
    the change-stream code path which doesn't re-validate
    subsequent stages until events flow — same shape mongod has —
    so the bad-spec rejection is only reachable when the stage
    runs standalone."""
    from pymongo.errors import OperationFailure

    coll = client["css_db"]["bad_spec"]
    coll.insert_one({"_id": 1})

    with pytest.raises(OperationFailure):
        client["css_db"].command(
            "aggregate",
            "bad_spec",
            pipeline=[{"$changeStreamSplitLargeEvent": "not-a-doc"}],
            cursor={},
        )


def test_large_event_actually_splits_into_fragments(client, server) -> None:
    """When ``splitLargeChangeStreamEvents`` is opted in (via the
    pipeline stage form) and the event genuinely exceeds 16 MB, the
    producer splits it into ``of: N`` fragments. Each fragment is
    a valid change event sharing the same ``_id`` resume token;
    heavy fields (>1 MB BSON each) are distributed one per
    fragment, light metadata (operationType, ns, etc.) is copied
    verbatim into every fragment.

    This pins mongo-rust-driver's
    ``test::change_stream::split_large_event`` semantics — a 10 MB
    pre-image + 10 MB ``$set`` value produces two fragments
    tagged ``{fragment: 1, of: 2}`` and ``{fragment: 2, of: 2}``.
    """
    db = client["split_db"]
    db.command("create", "coll", changeStreamPreAndPostImages={"enabled": True})
    coll = db["coll"]
    big_pre = "q" * (10 * 1024 * 1024)
    big_post = "z" * (10 * 1024 * 1024)
    coll.insert_one({"_id": 1, "value": big_pre})

    stream = coll.watch(
        pipeline=[{"$changeStreamSplitLargeEvent": {}}],
        full_document_before_change="required",
    )
    coll.update_one({"_id": 1}, {"$set": {"value": big_post}})

    import time

    events = []
    deadline = time.time() + 10
    while len(events) < 2 and time.time() < deadline:
        ev = stream.try_next()
        if ev is not None:
            events.append(ev)
    stream.close()

    assert len(events) == 2
    assert events[0]["splitEvent"] == {"fragment": 1, "of": 2}
    assert events[1]["splitEvent"] == {"fragment": 2, "of": 2}
    # Fragments share the same resume token (driver reassembles by _id).
    assert events[0]["_id"] == events[1]["_id"]
    # Each fragment is a valid change event: shared metadata copied
    # verbatim, heavy fields distributed one per fragment.
    for ev in events:
        assert ev["operationType"] == "update"
        assert "ns" in ev
        assert "documentKey" in ev
    # The heavy fields land in different fragments; together they
    # carry all the data the original event had.
    heavy_per_fragment = [
        (
            "updateDescription" in ev,
            "fullDocumentBeforeChange" in ev,
        )
        for ev in events
    ]
    # Exactly one fragment carries updateDescription; exactly one
    # carries fullDocumentBeforeChange.
    assert sum(h[0] for h in heavy_per_fragment) == 1
    assert sum(h[1] for h in heavy_per_fragment) == 1


def test_small_event_single_fragment_when_split_opted_in(client) -> None:
    """When the opt-in is set but the event is small, the producer
    emits a single ``{fragment: 1, of: 1}`` fragment — drivers see
    the ``splitEvent`` field on every event so they know the
    opt-in is honoured."""
    import time

    coll = client["split_db"]["small"]
    coll.insert_one({"_id": 1, "v": "seed"})

    stream = coll.watch(pipeline=[{"$changeStreamSplitLargeEvent": {}}])
    coll.insert_one({"_id": 2, "v": "tiny"})

    events = []
    deadline = time.time() + 5
    while not events and time.time() < deadline:
        ev = stream.try_next()
        if ev is not None:
            events.append(ev)
    stream.close()

    assert len(events) == 1
    assert events[0]["splitEvent"] == {"fragment": 1, "of": 1}


def test_split_stage_without_change_stream_first(client) -> None:
    """The stage alone (no preceding ``$changeStream``) is still
    accepted — it's a generic pass-through. Mongod's real
    implementation also tolerates it standalone (it's a no-op when
    upstream events aren't change-stream events)."""
    coll = client["css_db"]["standalone"]
    coll.insert_many([{"_id": i} for i in range(3)])

    # Standalone aggregate (not a change stream) with the split
    # stage as the only stage. Should return the input docs
    # unchanged.
    resp = client["css_db"].command(
        "aggregate",
        "standalone",
        pipeline=[{"$changeStreamSplitLargeEvent": {}}],
        cursor={},
    )
    assert resp["ok"] == 1.0
    docs = resp["cursor"]["firstBatch"]
    assert sorted(d["_id"] for d in docs) == [0, 1, 2]
