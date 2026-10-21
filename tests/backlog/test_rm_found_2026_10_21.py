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


@pytest.mark.backlog("RM-225")
@pytest.mark.parametrize("spec", [{"N": 2}, {"alpha": 0.75}], ids=["N", "alpha"])
def test_exp_moving_avg_over_null_missing_and_decimal(
    rust_mongo: Any, mongod: Any, spec: dict
) -> None:
    """mongod 8.2.11 averages over a null, a missing and a Decimal128 input
    (the null and missing rows answer null and do not move the average; a
    Decimal128 input turns the average decimal). The Rust server answers
    2 "not supported by the Rust server" for all of it."""
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {"e": {"$expMovingAvg": {"input": "$x", **spec}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    ours, theirs = _answers(rust_mongo, mongod, _EMA_DOCS, pipeline)
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


@pytest.mark.backlog("RM-226")
@pytest.mark.parametrize(("name", "stage"), _BAD_SPECS, ids=[n for n, _ in _BAD_SPECS])
def test_window_spec_errors_carry_mongods_codes(
    rust_mongo: Any, mongod: Any, name: str, stage: dict
) -> None:
    """mongod 8.2.11 refuses each spec with a code of its own: 605001,
    9 ("$shift does not accept a 'window' field"), 5339902 and 5429414. The
    Rust server answers 2 "not supported by the Rust server" for all four."""
    ours, theirs = _answers(rust_mongo, mongod, _SPEC_DOCS, [{"$setWindowFields": stage}])
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


@pytest.mark.backlog("RM-228")
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


@pytest.mark.backlog("RM-229")
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


def _retry_while_running(client: Any, n: int) -> tuple:
    """Send one retryable insert of `n` documents, and the same command again
    (same lsid, same txnNumber) on a second connection while the first is
    still running: what a driver does after a socket timeout. Returns both
    attempts' `ok`, `n` and first write errors."""
    import threading
    import time
    import uuid

    from bson import Binary, Int64

    host, port = client.address
    client.drop_database("rm230")
    cmd = {
        "insert": "c",
        "documents": [{"_id": i, "s": "p" * 300} for i in range(n)],
        "lsid": {"id": Binary(uuid.uuid4().bytes, 4)},
        "txnNumber": Int64(1),
        "$db": "rm230",
    }
    replies: list = []
    first = threading.Thread(target=lambda: replies.append(_op_msg(host, port, cmd)), daemon=True)
    first.start()
    # 30,000 documents keep the first attempt running well past this; with
    # few, it could finish first and the second would be a plain (and
    # correctly answered) retry of a finished write.
    time.sleep(0.3)
    replies.append(_op_msg(host, port, cmd))
    first.join(timeout=120)
    assert len(replies) == 2, "the first attempt never answered"
    # Both attempts, in either order: which one runs and which is answered
    # as the retry depends on which reaches the session first.
    return sorted(
        (r.get("ok"), r.get("n"), [(e["index"], e["code"]) for e in r.get("writeErrors", [])][:3])
        for r in replies
    )


@pytest.mark.backlog("RM-230")
def test_a_retry_waits_for_the_attempt_still_running(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a retry that arrives while its first attempt is still
    running waits for it (the session is checked out), then answers with the
    first attempt's result -- both answer `{n: 30000}`, no errors. The Rust
    server runs the two attempts side by side, and whichever comes second
    answers E11000 on the documents the other has already written: a
    driver that timed out reports a failed insert that in fact succeeded.
    Found when RM-187's regression test hit it on CI's slower runner (a 20 s
    socket timeout, then pymongo's retry)."""
    ours = _retry_while_running(rust_mongo, 30_000)
    theirs = _retry_while_running(mongod_rs, 30_000)
    assert theirs == [(1.0, 30_000, [])] * 2, theirs  # the reference, as measured
    assert ours == theirs


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
