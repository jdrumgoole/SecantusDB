"""Gap tests for the window-function divergences found on 2026-10-21, when
`tests/mongod_live/test_window_functions.py` first compared `$setWindowFields`
with mongod 8.2.11 (nothing in CI had: `windowfields.rs` had no unit tests,
and its only comparison was the retired Rust-vs-Python parity suite)."""

from __future__ import annotations

from typing import Any

import pytest
from bson import Decimal128, json_util

from tests.backlog.conftest import mongo_answer

_CANONICAL = json_util.JSONOptions(json_mode=json_util.JSONMode.CANONICAL)


def _answers(rust_mongo: Any, mongod: Any, docs: list, pipeline: list) -> tuple[str, str]:
    """Both servers' answers to `pipeline` over `docs`, as canonical extended
    JSON so a type or field-order difference is a difference."""
    out = []
    for client in (rust_mongo, mongod):
        coll = client["rm_found_1021"]["w"]
        coll.drop()
        coll.insert_many(docs)
        out.append(
            json_util.dumps(
                mongo_answer(lambda c=coll: list(c.aggregate(pipeline))), json_options=_CANONICAL
            )
        )
    return out[0], out[1]


_EMA_DOCS = [
    {"_id": 1, "x": 10},
    {"_id": 2, "x": None},
    {"_id": 3},
    {"_id": 4, "x": 4},
    {"_id": 5, "x": Decimal128("2.5")},
]


def _ema_answers(rust_mongo: Any, mongod: Any, spec: dict) -> tuple[str, str]:
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {"e": {"$expMovingAvg": {"input": "$x", **spec}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    return _answers(rust_mongo, mongod, _EMA_DOCS, pipeline)


@pytest.mark.backlog_fixed("RM-225")
@pytest.mark.parametrize("spec", [{"N": 2}, {"N": 3}, {"N": 1}], ids=["N 2", "N 3", "N 1"])
def test_exp_moving_avg_over_null_missing_and_decimal(
    rust_mongo: Any, mongod: Any, spec: dict
) -> None:
    """mongod 8.2.11 averages past an input that is not a number: a null or
    missing row answers the average UNCHANGED (null before the first numeric
    row) and does not move it, and a Decimal128 input makes the average
    decimal from that row on -- with mongod's own scale, which for an `N`
    comes from dividing `2/(N+1)` in Decimal128. The Rust server answered
    2 "not supported by the Rust server" for all of it (re-measured
    2026-11-06: this test's older docstring said the null rows answer null,
    which is true only of a LEADING one)."""
    ours, theirs = _ema_answers(rust_mongo, mongod, spec)
    assert ours == theirs


@pytest.mark.backlog_fixed("RM-225")
@pytest.mark.parametrize(
    "spec",
    [{"alpha": 0.75}, {"alpha": 0.5}, {"alpha": 0.001}, {"alpha": 0.999}],
    ids=["0.75", "0.5", "0.001", "0.999"],
)
def test_exp_moving_avg_with_an_alpha_over_a_decimal(
    rust_mongo: Any, mongod: Any, spec: dict
) -> None:
    """The smoothing factor given as `alpha` rather than `N` reaches the same
    values but a different SCALE, because an `alpha` double converts into the
    decimal accumulator at 15 significant digits while `N` divides exactly:
    `alpha: 0.5` over this corpus answers 30 decimals where `N: 3` answers
    two. Both are mongod's (measured 2026-11-06)."""
    ours, theirs = _ema_answers(rust_mongo, mongod, spec)
    assert ours == theirs


#: Shapes from the 1,170-case sweep of 2026-11-06 that the pre-fix server got
#: wrong in its DOUBLE answer -- mongod computes the average in Decimal128 and
#: converts, so `{N: 2}` over `[10, 0.1]` answers `3.4` where the same steps
#: in IEEE doubles answer `3.400000000000001`.
_DOUBLE_SCALE_DOCS: list[tuple[str, dict, list]] = [
    ("N 2 over 10 and 0.1", {"N": 2}, [10, 0.1]),
    ("N 100 over 10 and 0.1", {"N": 100}, [10, 0.1]),
    ("alpha 0.999 over 10 and 0.1", {"alpha": 0.999}, [10, 0.1]),
    ("N 2 over a long and 10", {"N": 2}, [2**40, 10]),
    ("alpha 0.1 over 4.0 and a long", {"alpha": 0.1}, [4.0, 2**40]),
]


@pytest.mark.backlog_fixed("RM-225")
@pytest.mark.parametrize(
    ("name", "spec", "xs"), _DOUBLE_SCALE_DOCS, ids=[n for n, _, _ in _DOUBLE_SCALE_DOCS]
)
def test_exp_moving_avg_doubles_come_from_the_decimal_average(
    rust_mongo: Any, mongod: Any, name: str, spec: dict, xs: list
) -> None:
    """mongod's double answer is its Decimal128 average converted, not a
    double computation, so it is free of the drift IEEE steps accumulate."""
    docs = [{"_id": i, "x": x} for i, x in enumerate(xs, start=1)]
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {"e": {"$expMovingAvg": {"input": "$x", **spec}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    ours, theirs = _answers(rust_mongo, mongod, docs, pipeline)
    assert ours == theirs


_SPEC_DOCS = [{"_id": i, "a": i, "b": i % 2, "y": i * 2} for i in range(1, 5)] + [
    {"_id": 5, "a": None, "b": 0, "y": 1}
]

#: (name, a `$setWindowFields` spec mongod refuses, with its own code).
_BAD_SPECS = [
    ("linearFill without a sortBy", {"output": {"f": {"$linearFill": "$y"}}}),
    (
        "shift with a window",
        {
            "sortBy": {"a": 1},
            "output": {
                "s": {
                    "$shift": {"output": "$y", "by": 1},
                    "window": {"documents": ["unbounded", "current"]},
                }
            },
        },
    ),
    (
        "range bounds with two sort keys",
        {"sortBy": {"a": 1, "b": 1}, "output": {"r": {"$sum": "$y", "window": {"range": [-1, 1]}}}},
    ),
    (
        "range window over a null sort value",
        {"sortBy": {"a": 1}, "output": {"r": {"$sum": "$y", "window": {"range": [-1, 1]}}}},
    ),
]


#: Shapes measured on 2026-11-06 while fixing RM-226, each with a code and
#: message of mongod's own. The last two of `_BAD_SPECS` plus these: the
#: precedence cases (a `window` on a spec that also lacks a `sortBy` names the
#: window) and the `$group`-only accumulators mongod refuses by name.
_MORE_BAD_SPECS: list[tuple[str, dict]] = [
    (
        "shift without a sortBy",
        {"output": {"s": {"$shift": {"output": "$y", "by": 1}}}},
    ),
    (
        "shift with a window and no sortBy",
        {
            "output": {
                "s": {
                    "$shift": {"output": "$y", "by": 1},
                    "window": {"documents": ["unbounded", "current"]},
                }
            }
        },
    ),
    (
        "linearFill with two sort keys",
        {"sortBy": {"a": 1, "b": 1}, "output": {"f": {"$linearFill": "$y"}}},
    ),
    (
        "linearFill with a window",
        {
            "sortBy": {"a": 1},
            "output": {
                "f": {"$linearFill": "$y", "window": {"documents": ["unbounded", "current"]}}
            },
        },
    ),
    (
        "locf with a window",
        {
            "sortBy": {"a": 1},
            "output": {"f": {"$locf": "$y", "window": {"documents": ["unbounded", "current"]}}},
        },
    ),
    (
        "range bounds over a descending sortBy",
        {"sortBy": {"a": -1}, "output": {"r": {"$sum": "$y", "window": {"range": [-1, 1]}}}},
    ),
    (
        "two functions in one output field",
        {"sortBy": {"a": 1}, "output": {"r": {"$sum": "$y", "$avg": "$y"}}},
    ),
    (
        "an unknown window function",
        {"sortBy": {"a": 1}, "output": {"r": {"$notAFunction": "$y"}}},
    ),
    (
        "mergeObjects as a window function",
        {"sortBy": {"a": 1}, "output": {"r": {"$mergeObjects": "$y"}}},
    ),
    ("a sortBy that is not a document", {"sortBy": "a", "output": {"r": {"$sum": "$y"}}}),
]

_ALL_BAD_SPECS = _BAD_SPECS + _MORE_BAD_SPECS


@pytest.mark.backlog_fixed("RM-226")
@pytest.mark.parametrize(("name", "stage"), _ALL_BAD_SPECS, ids=[n for n, _ in _ALL_BAD_SPECS])
def test_window_spec_errors_carry_mongods_codes(
    rust_mongo: Any, mongod: Any, name: str, stage: dict
) -> None:
    """Each spec is refused with mongod's own code and message: 605001 and
    5339902 and 8947401 at parse time, 5429414 from the executor, 14 for a
    wrong-typed `sortBy`, and 9 for the rest. The Rust server answered
    2 "not supported by the Rust server" for all of them."""
    ours, theirs = _answers(rust_mongo, mongod, _SPEC_DOCS, [{"$setWindowFields": stage}])
    assert ours == theirs


#: Out of insertion order on `a`, with a null and a missing `y`, so a
#: carry-forward and an order difference both show.
_ACCEPTED_DOCS = [
    {"_id": 1, "a": 3, "y": 1},
    {"_id": 2, "a": 1, "y": None},
    {"_id": 3, "a": 2},
    {"_id": 4, "a": 5, "y": 7},
]


@pytest.mark.backlog_fixed("RM-367")
@pytest.mark.parametrize(
    "stage",
    [{"sortBy": {"a": 1}, "output": {}}, {"output": {}}],
    ids=["with a sortBy", "without a sortBy"],
)
def test_an_empty_output_passes_the_documents_through(
    rust_mongo: Any, mongod: Any, stage: dict
) -> None:
    """mongod 8.2.11 accepts an empty `output`: the documents come back
    unchanged, in sortBy order when the stage has one and input order when it
    does not (measured 2026-11-06). The Rust server refused the stage with
    2 "not supported by the Rust server"."""
    ours, theirs = _answers(rust_mongo, mongod, _ACCEPTED_DOCS, [{"$setWindowFields": stage}])
    assert ours == theirs


@pytest.mark.backlog_fixed("RM-368")
@pytest.mark.parametrize(
    "stage",
    [
        {"output": {"f": {"$locf": "$y"}}},
        {"partitionBy": {"$mod": ["$a", 2]}, "output": {"f": {"$locf": "$y"}}},
    ],
    ids=["one partition", "partitioned"],
)
def test_locf_needs_no_sort_by(rust_mongo: Any, mongod: Any, stage: dict) -> None:
    """`$locf` takes no `sortBy` on mongod 8.2.11: it carries the last
    observation forward in the partition's own order, and a leading row with
    nothing to carry answers null (measured 2026-11-06). The Rust server
    required a `sortBy` and refused the stage with 2 "not supported by the
    Rust server"."""
    ours, theirs = _answers(rust_mongo, mongod, _ACCEPTED_DOCS, [{"$setWindowFields": stage}])
    assert ours == theirs


_SHIFT_DOCS = [{"_id": 1, "x": 1}, {"_id": 2}, {"_id": 3, "x": None}, {"_id": 4, "x": 4}]


@pytest.mark.backlog_fixed("RM-227")
@pytest.mark.parametrize(
    "spec",
    [
        {"output": "$x", "by": -1, "default": "D"},
        {"output": "$x", "by": -1},
        {"output": {"$getField": "nosuch"}, "by": 1, "default": "D"},
        {"output": "$x", "by": 0, "default": "D"},
    ],
    ids=["absent field", "no default", "missing expression", "by 0"],
)
def test_shift_default_replaces_a_missing_output(rust_mongo: Any, mongod: Any, spec: dict) -> None:
    """mongod 8.2.11: when `output` evaluates to MISSING on the shifted row,
    `$shift` answers its `default` (null without one); an explicit null stays
    null. The Rust server answered null for a missing output."""
    pipeline = [
        {"$setWindowFields": {"sortBy": {"_id": 1}, "output": {"s": {"$shift": spec}}}},
        {"$project": {"s": 1}},
    ]
    ours, theirs = _answers(rust_mongo, mongod, _SHIFT_DOCS, pipeline)
    assert ours == theirs


#: `$convert` targets mongod refuses or answers with `onError`, each with and
#: without an `onError` (measured on mongod 8.2.11, 2026-10-21).
_CONVERT_TARGETS = ["uuid", "foo", "array", "timestamp", "null", "regex", 99, 4, {"type": "uuid"}]


@pytest.mark.backlog_fixed("RM-228")
@pytest.mark.parametrize("on_error", [False, True], ids=["plain", "onError"])
@pytest.mark.parametrize("to", _CONVERT_TARGETS, ids=[repr(t) for t in _CONVERT_TARGETS])
def test_convert_to_a_target_it_cannot_produce(
    rust_mongo: Any, mongod: Any, to: Any, on_error: bool
) -> None:
    """mongod 8.2.11: an unknown type name answers 2 "Unknown type name: X"
    and a number that is no BSON type 9, `onError` or not; a real BSON type
    `$convert` cannot produce answers 241 "Unsupported conversion from string
    to array in $convert with no onError value", or the `onError` value when
    one is given. The Rust server answers 2 "not supported by the Rust
    server" for every one of them."""
    spec: dict[str, Any] = {"input": "$x", "to": to}
    if on_error:
        spec["onError"] = "E"
    pipeline = [{"$project": {"_id": 0, "r": {"$convert": spec}}}]
    ours, theirs = _answers(rust_mongo, mongod, [{"_id": 1, "x": "5"}], pipeline)
    assert ours == theirs


@pytest.mark.backlog_fixed("RM-229")
@pytest.mark.parametrize("full_document", ["whenAvailable", "required"])
def test_change_stream_serves_the_stored_post_image(
    rust_mongo: Any, mongod_rs: Any, full_document: str
) -> None:
    """mongod 8.2.11, on a collection created with
    `changeStreamPreAndPostImages: {enabled: true}`: an update event's
    `fullDocument` is the post-image stored when that update ran -- `a: 2`
    then `a: 3` here. The Rust server looks the document up as it is when
    the event is read (`a: 3` both times); once a later write deletes it,
    "whenAvailable" answers null and "required" fails the stream (47)."""
    answers = []
    for client in (rust_mongo, mongod_rs):
        client.drop_database("rm229")
        db = client["rm229"]
        db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
        db.c.insert_one({"_id": 1, "a": 1})

        def read(db: Any = db) -> list:
            with db.c.watch(full_document=full_document, max_await_time_ms=200) as stream:
                db.c.update_one({"_id": 1}, {"$set": {"a": 2}})
                db.c.update_one({"_id": 1}, {"$set": {"a": 3}})
                out = []
                for _ in range(50):
                    ev = stream.try_next()
                    if ev is not None:
                        out.append(ev.get("fullDocument"))
                    if len(out) == 2:
                        break
                return out

        answers.append(mongo_answer(read))
    assert answers[0] == answers[1]


def _images_left_by_updates(client: Any, full_document: str) -> Any:
    """Two updates and a delete, then images disabled and one more update,
    all read back from before the first write."""
    client.drop_database("rm229b")
    db = client["rm229b"]
    db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
    start = client.admin.command("ping")["operationTime"]
    db.c.insert_one({"_id": 1, "a": {"b": 1}, "x": 1})
    db.c.update_one({"_id": 1}, {"$set": {"a.b": 2, "n": 1}, "$unset": {"x": 1}})
    db.c.update_one({"_id": 1}, {"$set": {"x": 3}})
    db.c.delete_one({"_id": 1})
    db.command("collMod", "c", changeStreamPreAndPostImages={"enabled": False})
    db.c.insert_one({"_id": 2})
    db.c.update_one({"_id": 2}, {"$set": {"a": 1}})

    def read() -> list:
        out = []
        with db.c.watch(full_document=full_document, start_at_operation_time=start) as stream:
            while (ev := stream.try_next()) is not None:
                out.append((ev["operationType"], ev.get("fullDocument", "absent")))
        return out

    got = mongo_answer(read)
    if got[0] == "err":
        # The message names the event's clusterTime, which is each server's own.
        import re

        got = (got[0], got[1], re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(t, i)", got[2]))
    return got


@pytest.mark.backlog_fixed("RM-229")
@pytest.mark.parametrize("full_document", ["whenAvailable", "required"])
def test_a_post_image_outlives_the_document(
    rust_mongo: Any, mongod_rs: Any, full_document: str
) -> None:
    """mongod 8.2.11 builds the post-image from the pre-image it stored and
    the update: a later delete leaves both updates' images, a field-order
    change included (`n` after `a`, `x` re-added last), and an update made
    while images were disabled has none (null, or 47 for "required"). That
    images are disabled when the stream is READ does not take away the
    images stored before."""
    assert _images_left_by_updates(rust_mongo, full_document) == _images_left_by_updates(
        mongod_rs, full_document
    )


def _batches_to_a_missing_image(client: Any, batch_size: int) -> list:
    """The `aggregate` and `getMore`s of a "required" stream whose second
    event has no post-image, as `(command, event types)` or `(command, code)`."""
    client.drop_database("rm229c")
    db = client["rm229c"]
    db.create_collection("c", changeStreamPreAndPostImages={"enabled": False})
    start = client.admin.command("ping")["operationTime"]
    db.c.insert_one({"_id": 1})
    db.c.update_one({"_id": 1}, {"$set": {"a": 1}})
    stage = {"$changeStream": {"fullDocument": "required", "startAtOperationTime": start}}
    try:
        reply = db.command(
            {"aggregate": "c", "pipeline": [stage], "cursor": {"batchSize": batch_size}}
        )
    except Exception as e:
        return [("aggregate", getattr(e, "code", None))]
    out = [("aggregate", [e["operationType"] for e in reply["cursor"]["firstBatch"]])]
    for _ in range(3):
        try:
            more = db.command({"getMore": reply["cursor"]["id"], "collection": "c", "batchSize": 1})
        except Exception as e:
            out.append(("getMore", getattr(e, "code", None)))
            break
        out.append(("getMore", [e["operationType"] for e in more["cursor"]["nextBatch"]]))
    return out


@pytest.mark.backlog_fixed("RM-229")
@pytest.mark.parametrize("batch_size", [0, 1, 2, 101])
def test_a_missing_required_image_fails_the_batch_that_reaches_it(
    rust_mongo: Any, mongod_rs: Any, batch_size: int
) -> None:
    """mongod 8.2.11 fails the whole command whose batch reaches the event
    with no image: the `aggregate` itself when its first batch does (47,
    under the aggregate's prefix), and a batch that fills first is returned,
    leaving the error to the next getMore. The Rust server returned the
    events before it and failed only the getMore after."""
    assert _batches_to_a_missing_image(rust_mongo, batch_size) == _batches_to_a_missing_image(
        mongod_rs, batch_size
    )


def _op_msg(host: str, port: int, command: dict) -> dict:
    """One command as a raw OP_MSG on a connection of its own. A driver
    would replace the `lsid` with a session of its own, and the retry is
    only a retry if both attempts carry the same one."""
    import socket
    import struct

    import bson

    body = b"\x00" + bson.encode(command)
    msg = struct.pack("<iiii", 16 + 4 + len(body), 1, 0, 2013) + struct.pack("<I", 0) + body
    with socket.create_connection((host, port), timeout=120) as sock:
        sock.sendall(msg)
        head = b""
        while len(head) < 4:
            head += sock.recv(4 - len(head))
        (length,) = struct.unpack("<i", head)
        rest = b""
        while len(rest) < length - 4:
            rest += sock.recv(length - 4 - len(rest))
    return bson.decode(rest[12 + 4 + 1 :])


def _slow_update(host: str, port: int, db: str) -> dict:
    """An update of `{_id: 0}` whose filter folds `$outer` ranges of
    `$inner` numbers, with `$outer` set so it runs about two seconds on this
    server: mongod is a hundred times faster than a debug build, so no one
    size keeps both busy. The fields stop mongod folding it to a constant."""
    import time

    _op_msg(host, port, {"dropDatabase": 1, "$db": db})
    update = {
        "q": {
            "_id": 0,
            "$expr": {
                "$gte": [
                    {
                        "$reduce": {
                            "input": {"$range": [0, "$outer"]},
                            "initialValue": 0,
                            "in": {"$add": ["$$value", {"$size": {"$range": [0, "$inner"]}}]},
                        }
                    },
                    0,
                ]
            },
        },
        "u": {"$inc": {"n": 1}},
    }
    outer = 1_000
    while True:
        row = {"_id": 0, "n": 0, "outer": outer, "inner": 1_000}
        _op_msg(host, port, {"delete": "c", "deletes": [{"q": {}, "limit": 0}], "$db": db})
        _op_msg(host, port, {"insert": "c", "documents": [row], "$db": db})
        began = time.monotonic()
        _op_msg(host, port, {"update": "c", "updates": [update], "$db": db})
        took = time.monotonic() - began
        if took > 0.2:
            break
        outer *= 10
    outer = int(outer * 2.0 / took)
    # One timing on a busy machine overstates the cost, and the update then
    # runs far under two seconds once the load passes: the attempt a test
    # means to catch running ends before it is caught. Time the calibrated
    # update itself, and lengthen it until it holds a second.
    for _ in range(5):
        row = {"_id": 0, "n": 0, "outer": outer, "inner": 1_000}
        _op_msg(host, port, {"delete": "c", "deletes": [{"q": {}, "limit": 0}], "$db": db})
        _op_msg(host, port, {"insert": "c", "documents": [row], "$db": db})
        began = time.monotonic()
        _op_msg(host, port, {"update": "c", "updates": [update], "$db": db})
        took = time.monotonic() - began
        if took >= 1.0:
            break
        outer = int(outer * 2.0 / took)
    row = {"_id": 0, "n": 0, "outer": outer, "inner": 1_000}
    _op_msg(host, port, {"delete": "c", "deletes": [{"q": {}, "limit": 0}], "$db": db})
    _op_msg(host, port, {"insert": "c", "documents": [row], "$db": db})
    return update


def _retry_while_running(client: Any) -> tuple:
    """Send one slow retryable update, and the same command again (same
    lsid, same txnNumber) on a second connection while the first is still
    running: what a driver does after a socket timeout. Returns both
    attempts' `ok`, `n`, `nModified` and first write errors, and how many
    times the update applied."""
    import threading
    import time
    import uuid

    from bson import Binary, Int64

    host, port = client.address
    cmd = {
        "update": "c",
        "updates": [_slow_update(host, port, "rm230")],
        "lsid": {"id": Binary(uuid.uuid4().bytes, 4)},
        "txnNumber": Int64(1),
        "$db": "rm230",
    }
    replies: list = []
    first_ended: list = []

    def first() -> None:
        replies.append(_op_msg(host, port, cmd))
        first_ended.append(time.monotonic())

    thread = threading.Thread(target=first, daemon=True)
    thread.start()
    # Retry once the first attempt runs: a fixed pause raced it on a loaded
    # box, either side.
    running = {"aggregate": 1, "cursor": {}, "$db": "admin"}
    running["pipeline"] = [{"$currentOp": {}}, {"$match": {"ns": "rm230.c", "op": "update"}}]
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and not first_ended:
        if _op_msg(host, port, running)["cursor"]["firstBatch"]:
            break
        time.sleep(0.05)
    sent = time.monotonic()
    replies.append(_op_msg(host, port, cmd))
    thread.join(timeout=120)
    assert len(replies) == 2, "the first attempt never answered"
    if first_ended[0] <= sent:
        # Not an assertion: the retry was not concurrent, so this measured
        # nothing, and the verifier must say BROKEN rather than FIXED.
        raise RuntimeError("the first attempt finished before the retry was sent")
    applied = _op_msg(host, port, {"find": "c", "$db": "rm230"})["cursor"]["firstBatch"][0]["n"]
    return sorted(
        (
            r.get("ok"),
            r.get("n"),
            r.get("nModified"),
            [(e["index"], e["code"]) for e in r.get("writeErrors", [])][:3],
        )
        for r in replies
    ), applied


@pytest.mark.backlog_fixed("RM-230")
def test_a_retry_waits_for_the_attempt_still_running(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a retry that arrives while its first attempt is still
    running waits for it (the session is checked out), then answers with the
    first attempt's result -- both answer `{n: 1, nModified: 1}`, and the
    update applied once. The Rust server ran the two attempts side by side:
    a retried insert answered E11000 on the documents the other had
    written, so a driver that timed out reported a failed insert that had
    succeeded. Found when RM-187's regression test hit it on CI's slower
    runner (a 20 s socket timeout, then pymongo's retry)."""
    theirs = _retry_while_running(mongod_rs)
    assert theirs == ([(1.0, 1, 1, [])] * 2, 1), theirs  # the reference, as measured
    assert _retry_while_running(rust_mongo) == theirs


@pytest.mark.backlog_fixed("RM-231")
def test_std_dev_window_rounds_as_the_linux_build(rust_mongo: Any) -> None:
    """mongod's Linux x86-64 build -- the exemplar (Joe, 2026-10-21) --
    computes a window's `count * x - sum` with two roundings; its macOS
    arm64 build fuses it into one, and the last digit differs. Recorded
    from mongod 8.2.12 on CI's Linux runner (2026-10-21): the `$stdDevSamp`
    of `documents: [-2, 0]` windows over these six doubles. macOS mongod
    answers 184.0386523296408, 463.51949769953535 and 489.1562645109442 for
    the last three."""
    import random

    rng = random.Random(3)
    xs = [[rng.uniform(-1e3, 1e3) for _ in range(rng.randint(3, 10))] for _ in range(20)][4]
    pipeline = [
        {"$documents": [{"i": j, "v": x} for j, x in enumerate(xs)]},
        {
            "$setWindowFields": {
                "sortBy": {"i": 1},
                "output": {"s": {"$stdDevSamp": "$v", "window": {"documents": [-2, 0]}}},
            }
        },
    ]
    got = [d["s"] for d in rust_mongo.admin.aggregate(pipeline)]
    assert got == [
        None,
        585.5692783309847,
        550.3405014627988,
        184.03865232964097,
        463.5194976995354,
        489.15626451094425,
    ]
