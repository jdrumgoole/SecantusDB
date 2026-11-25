"""Change-stream resume tokens in mongod 8.2's own form (storage-model 4b-4).

A token's `_data` is the KeyString of `{clusterTime, version, tokenType,
txnOpIndex, fromInvalidate, uuid, eventIdentifier, fragmentNum?}` and
`_typeBits` is that KeyString's type bits. The same writes on both servers
therefore make tokens equal in every byte except the two things that name a
server's own state, the cluster time and the collection UUIDs; masking those,
`_data` and `_typeBits` are compared whole, so a field out of place, a number
of another type or an identifier built differently is a difference. (Our
encoder is also checked against the captured tokens, byte for byte, in
`crates/secantus-storage/tests/resume_tokens.rs`.)

Resuming is compared too: from each token, both servers must give the same
next events, and the same answer for a token that names no event of the
stream (a changed identifier, a `txnOpIndex` past the entry, a time with no
entry, another collection's).
"""

from __future__ import annotations

import re
import time
from typing import Any

from bson import Int64, Timestamp

from tests.backlog.conftest import mongo_answer
from tests.mongod_live.conftest import same

_UUID = re.compile(r"5A1004[0-9A-F]{32}")


def _mask(token: dict) -> tuple:
    """A token with the server's own clusterTime and collection UUIDs masked:
    `(_data, _typeBits)`."""
    data = token["_data"]
    assert data.startswith("82"), data
    data = "82<ts>" + _UUID.sub("5A1004<uuid>", data[18:])
    bits = token.get("_typeBits")
    return (data, bits.hex().upper() if bits is not None else None)


def _time(token: dict) -> Timestamp:
    """The clusterTime a token starts with."""
    data = token["_data"]
    return Timestamp(int(data[2:10], 16), int(data[10:18], 16))


def _uuid(token: dict) -> str | None:
    m = _UUID.search(token["_data"])
    return m.group(0)[6:].lower() if m else None


def _read(stream: Any, want: int, seconds: float = 20.0) -> list:
    out, end = [], time.monotonic() + seconds
    while len(out) < want and time.monotonic() < end:
        ev = stream.try_next()
        if ev is not None:
            out.append(ev)
    return out


def _fresh(client: Any, name: str) -> Any:
    client.drop_database(name)
    return client[name]


def _check_against_the_event(events: list) -> None:
    """A token's own time is its event's clusterTime, and the UUID in it is
    the collection's (`showExpandedEvents` shows it)."""
    for ev in events:
        if ev["operationType"] == "invalidate":
            continue
        assert _time(ev["_id"]) == ev["clusterTime"], ev
        coll_uuid = ev.get("collectionUUID")
        assert _uuid(ev["_id"]) == (coll_uuid.hex() if coll_uuid else None), ev


def _summary(events: list) -> list:
    return [(e.get("operationType"), e.get("splitEvent"), _mask(e["_id"])) for e in events]


# ------------------------------------------------------------- CRUD kinds


_IDS = [1, Int64(2**40), 1.5, "str", {"a": 1, "b": 2}, {"a": Int64(5), "b": 2.5}, None]


def _crud(client: Any) -> list:
    db = _fresh(client, "live_rt_crud")
    db.create_collection("c")
    with db.c.watch(show_expanded_events=True) as s:
        for i in _IDS:
            db.c.insert_one({"_id": i, "x": 1})
        db.c.update_one({"_id": 1}, {"$set": {"x": 2}})
        db.c.replace_one({"_id": 1}, {"y": 1})
        db.c.delete_one({"_id": 1})
        events = _read(s, len(_IDS) + 3)
    assert len(events) == len(_IDS) + 3
    _check_against_the_event(events)
    return events


def test_crud_tokens_match_mongod(rust_mongo: Any, mongod_rs: Any) -> None:
    ours, theirs = _crud(rust_mongo), _crud(mongod_rs)
    same(_summary(ours), _summary(theirs))
    # The types of `_id` made type bits: not vacuous.
    assert any(t[2][1] for t in _summary(theirs))


def test_database_and_cluster_scopes_make_the_same_token(rust_mongo: Any, mongod_rs: Any) -> None:
    def tokens(client: Any) -> list:
        db = _fresh(client, "live_rt_scopes")
        db.create_collection("c")
        out = []
        with db.c.watch() as c, db.watch() as d, client.watch() as g:
            db.c.insert_one({"_id": 1})
            db.c.insert_one({"_id": 2})
            for s in (c, d, g):
                evs = [e for e in _read(s, 2) if e["ns"]["db"] == "live_rt_scopes"]
                out.append([_mask(e["_id"]) for e in evs])
        return out

    ours, theirs = tokens(rust_mongo), tokens(mongod_rs)
    same(ours, theirs)
    assert ours[0] == ours[1] == ours[2]


# ------------------------------------------------------ txnOpIndex, batches


def _batches(client: Any) -> list:
    db = _fresh(client, "live_rt_txn")
    db.create_collection("c")
    with db.c.watch() as s:
        db.c.insert_many([{"_id": 100}, {"_id": 101}, {"_id": 102}])
        with client.start_session() as ses, ses.start_transaction():
            db.c.insert_one({"_id": 200}, session=ses)
            db.c.update_one({"_id": 200}, {"$set": {"q": 1}}, session=ses)
            db.c.delete_one({"_id": 200}, session=ses)
        events = _read(s, 6)
    assert len(events) == 6
    return events


def test_events_of_an_apply_ops_carry_their_index(rust_mongo: Any, mongod_rs: Any) -> None:
    ours, theirs = _batches(rust_mongo), _batches(mongod_rs)
    same(_summary(ours), _summary(theirs))
    # Three events share one time and are told apart by txnOpIndex.
    for evs in (ours, theirs):
        assert evs[0]["clusterTime"] == evs[1]["clusterTime"] == evs[2]["clusterTime"]
        assert len({e["_id"]["_data"] for e in evs[:3]}) == 3


# ------------------------------------------------------------------- DDL


def _ddl(client: Any) -> list:
    name = "live_rt_ddl"
    db = _fresh(client, name)
    import pymongo

    with db.watch(show_expanded_events=True) as s:
        db.create_collection("e1", capped=True, size=100000)
        db.create_collection("e2", validator={"x": {"$gt": 0}})
        db.e1.create_indexes(
            [pymongo.IndexModel("a", unique=True), pymongo.IndexModel([("b", 1), ("c", -1)])]
        )
        db.command("collMod", "e2", validator={"y": {"$gt": 0}}, validationLevel="moderate")
        db.e1.drop_index("b_1_c_-1")
        db.e1.rename("e3")
        db.create_collection("e4")
        db.e3.rename("e4", dropTarget=True)
        db.e4.drop()
        client.drop_database(name)
        events = _read(s, 14, seconds=30)
    return events


def test_ddl_tokens_match_mongod(rust_mongo: Any, mongod_rs: Any) -> None:
    ours, theirs = _ddl(rust_mongo), _ddl(mongod_rs)
    assert [e["operationType"] for e in theirs][-2:] == ["dropDatabase", "invalidate"]
    same(_summary(ours), _summary(theirs))
    _check_against_the_event(
        [e for e in ours if "collectionUUID" in e or e["operationType"] == "dropDatabase"]
    )


def test_a_rename_token_names_its_description_on_a_stream_without_expanded_events(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod's identifier holds `operationDescription` (`to`, `dropTarget`)
    whether or not the stream shows it."""

    def tokens(client: Any) -> list:
        db = _fresh(client, "live_rt_rename")
        db.create_collection("a")
        db.create_collection("b")
        with db.watch() as s:
            db.a.rename("b", dropTarget=True)
            db.b.rename("c")
            return [_mask(e["_id"]) for e in _read(s, 2)]

    same(tokens(rust_mongo), tokens(mongod_rs))


def _invalidate(client: Any) -> list:
    db = _fresh(client, "live_rt_inv")
    db.create_collection("c")
    out = []
    with db.c.watch(show_expanded_events=True) as s:
        db.c.insert_one({"_id": 1})
        db.c.drop()
        out.extend(_read(s, 3))
    with db.watch(show_expanded_events=True) as s:
        db.create_collection("h")
        client.drop_database("live_rt_inv")
        out.extend(_read(s, 4))
    return out


def test_invalidate_token_is_the_triggering_events_with_from_invalidate(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    ours, theirs = _invalidate(rust_mongo), _invalidate(mongod_rs)
    same(_summary(ours), _summary(theirs))
    kinds = [e["operationType"] for e in theirs]
    assert kinds.count("invalidate") == 2, kinds
    for evs in (ours, theirs):
        for i, ev in enumerate(evs):
            if ev["operationType"] == "invalidate":
                # The same bytes but the one `fromInvalidate` bool.
                trigger = evs[i - 1]["_id"]["_data"]
                assert ev["_id"]["_data"] != trigger
                # The flag's own byte (`29 6F`), not the first `6F` the data
                # holds: the clock's seconds, in the token's first bytes, hold
                # one for an hour in every few (`6B06F859`) and the test
                # failed then.
                flagged = re.sub(r"(2B042C0100296)F", r"\1E", ev["_id"]["_data"], count=1)
                assert flagged == trigger


# ---------------------------------------------------------- split events


def _split(client: Any) -> list:
    db = _fresh(client, "live_rt_split")
    db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
    db.c.insert_one({"_id": 1})
    big = "x" * (9 * 1024 * 1024)
    with db.c.watch(
        [{"$changeStreamSplitLargeEvent": {}}],
        full_document_before_change="required",
        max_await_time_ms=500,
    ) as s:
        db.c.update_one({"_id": 1}, {"$set": {"big": big}})
        db.c.update_one({"_id": 1}, {"$set": {"big2": big}, "$unset": {"big": ""}})
        return _read(s, 3, seconds=60)


def test_split_fragments_carry_a_fragment_number(rust_mongo: Any, mongod_rs: Any) -> None:
    ours, theirs = _split(rust_mongo), _split(mongod_rs)
    assert [e.get("splitEvent") for e in theirs] == [
        None,
        {"fragment": 1, "of": 2},
        {"fragment": 2, "of": 2},
    ]
    same(_summary(ours), _summary(theirs))


def test_resume_from_a_fragment_continues_after_it(rust_mongo: Any, mongod_rs: Any) -> None:
    def after(client: Any) -> list:
        db = _fresh(client, "live_rt_split_resume")
        db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
        db.c.insert_one({"_id": 1})
        big = "x" * (9 * 1024 * 1024)
        pipeline = [{"$changeStreamSplitLargeEvent": {}}]
        with db.c.watch(
            pipeline, full_document_before_change="required", max_await_time_ms=500
        ) as s:
            db.c.update_one({"_id": 1}, {"$set": {"big": big}})
            db.c.update_one({"_id": 1}, {"$set": {"big2": big}, "$unset": {"big": ""}})
            db.c.insert_one({"_id": 2})
            events = _read(s, 4, seconds=60)
        assert len(events) == 4, events
        out = []
        for ev in events[1:3]:
            with db.c.watch(
                pipeline,
                resume_after=ev["_id"],
                full_document_before_change="required",
                max_await_time_ms=500,
            ) as s:
                out.append([(e.get("splitEvent"), e.get("documentKey")) for e in _read(s, 2)])
        return out

    same(after(rust_mongo), after(mongod_rs))


# ------------------------------------------------------ high-water marks


def test_high_water_marks_are_a_time_and_nothing_else(rust_mongo: Any, mongod_rs: Any) -> None:
    def marks(client: Any) -> list:
        db = _fresh(client, "live_rt_hwm")
        db.create_collection("a")
        out = []
        r = db.command("aggregate", "a", pipeline=[{"$changeStream": {}}], cursor={})
        out.append(("open", _mask(r["cursor"]["postBatchResumeToken"]), r["cursor"]["firstBatch"]))
        cid = r["cursor"]["id"]
        db.other.insert_one({"_id": 1})
        g = db.command("getMore", cid, collection="a", maxTimeMS=300)
        out.append(("after a write elsewhere", _mask(g["cursor"]["postBatchResumeToken"])))
        return out

    ours, theirs = marks(rust_mongo), marks(mongod_rs)
    same(ours, theirs)
    assert theirs[0][1][0] == "82<ts>2B0429296E1404"


def test_a_new_streams_mark_is_the_next_time_and_a_scanned_one_the_last(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """Measured 8.2.11: a stream opened now has `last entry + 1`; after
    reading past entries that are no event of it, the last entry read."""

    def run(client: Any) -> tuple:
        db = _fresh(client, "live_rt_hwm2")
        db.create_collection("a")
        db.other.insert_one({"_id": 0})
        last = client.local["oplog.rs"].find().sort("$natural", -1).limit(1)[0]["ts"]
        r = db.command("aggregate", "a", pipeline=[{"$changeStream": {}}], cursor={})
        opened = _time(r["cursor"]["postBatchResumeToken"])
        cid = r["cursor"]["id"]
        db.other.insert_one({"_id": 1})
        last2 = client.local["oplog.rs"].find().sort("$natural", -1).limit(1)[0]["ts"]
        g = db.command("getMore", cid, collection="a", maxTimeMS=300)
        scanned = _time(g["cursor"]["postBatchResumeToken"])
        return (
            (opened.time - last.time, opened.inc - last.inc),
            (scanned.time - last2.time, scanned.inc - last2.inc),
        )

    ours, theirs = run(rust_mongo), run(mongod_rs)
    assert theirs == ((0, 1), (0, 0))
    same(ours, theirs)


# --------------------------------------------------------------- resuming


def _resume_streams(client: Any) -> dict:
    """The same writes, then a resume from the token of each event and from
    tokens made from them that name no event of the stream."""
    db = _fresh(client, "live_rt_resume")
    db.create_collection("c")
    db.create_collection("other")
    with db.c.watch() as s:
        db.c.insert_one({"_id": 1})
        db.c.insert_one({"_id": 2})
        db.c.insert_one({"_id": 3})
        with client.start_session() as ses, ses.start_transaction():
            for i in (10, 11, 12):
                db.c.insert_one({"_id": i}, session=ses)
        db.c.insert_one({"_id": 4})
        events = _read(s, 7)
    assert len(events) == 7
    toks = [e["_id"] for e in events]

    def resume(label: str, **kw: Any) -> tuple:
        def run() -> list:
            with db.c.watch(max_await_time_ms=100, **kw) as st:
                got = _read(st, 100, seconds=1.0)
                return [(e["operationType"], e["documentKey"]) for e in got]

        a = mongo_answer(run)
        if a[0] == "err":
            # The message names the token found in its place: per-server.
            a = (a[0], a[1], re.sub(r"_data: \"[0-9A-F]+\"", "_data: <token>", a[2]))
        return (label, a)

    def hwm(tok: dict) -> dict:
        return {"_data": tok["_data"][:18] + "2B0429296E1404"}

    def edit(tok: dict, old: str, new: str) -> dict:
        assert old in tok["_data"], (old, tok)
        out = dict(tok)
        out["_data"] = tok["_data"].replace(old, new, 1)
        return out

    def later(tok: dict, inc: int) -> dict:
        n = int(tok["_data"][10:18], 16) + inc
        return {**tok, "_data": tok["_data"][:10] + f"{n:08X}" + tok["_data"][18:]}

    out = {}
    for i, tok in enumerate(toks):
        out[f"resumeAfter event {i}"] = resume(f"after {i}", resume_after=tok)
    out["startAfter"] = resume("startAfter 1", start_after=toks[1])
    out["hwm at event 1"] = resume("hwm 1", resume_after=hwm(toks[1]))
    out["hwm at txn"] = resume("hwm 3", resume_after=hwm(toks[3]))
    out["other documentKey"] = resume(
        "dk", resume_after=edit(toks[1], "461E5F6964002B04", "461E5F6964002B06")
    )
    out["txnOpIndex past the entry"] = resume(
        "idx", resume_after=edit(toks[3], "2C0100296E", "2C01002B0A6E")
    )
    out["txnOpIndex of another event"] = resume(
        "idx2", resume_after=edit(toks[4], "2C01002B026E", "2C0100296E")
    )
    out["a time with no entry"] = resume("time", resume_after=later(toks[1], 1))
    out["a time not reached"] = resume("future", resume_after=later(toks[1], 1 << 40))
    out["a future high-water mark"] = resume(
        "futurehwm", resume_after={"_data": "82FFFFFF0000000001" + "2B0429296E1404"}
    )
    out["a null uuid"] = resume(
        "nulluuid", resume_after=edit(toks[0], _UUID.search(toks[0]["_data"]).group(0), "14")
    )
    out["start at the time"] = resume("startAt", start_at_operation_time=events[1]["clusterTime"])
    return out


def test_resuming_from_every_token_matches_mongod(rust_mongo: Any, mongod_rs: Any) -> None:
    ours, theirs = _resume_streams(rust_mongo), _resume_streams(mongod_rs)
    # Not vacuous: the reference resumed, and refused.
    assert (
        theirs["resumeAfter event 0"][1][0] == "ok"
        and len(theirs["resumeAfter event 0"][1][1]) == 6
    )
    assert theirs["other documentKey"][1][:2] == ("err", 280)
    assert theirs["a null uuid"][1][:2] == ("err", 260)
    assert theirs["hwm at event 1"][1][1][0] == ("insert", {"_id": 2})
    same(ours, theirs)


def test_resume_from_the_token_of_an_invalidate(rust_mongo: Any, mongod_rs: Any) -> None:
    def run(client: Any) -> list:
        db = _fresh(client, "live_rt_inv_resume")
        db.create_collection("c")
        with db.c.watch() as s:
            db.c.insert_one({"_id": 1})
            db.c.drop()
            events = _read(s, 3)
        inv = events[-1]["_id"]
        assert events[-1]["operationType"] == "invalidate"
        db.create_collection("c")
        db.c.insert_one({"_id": 2})
        out = [mongo_answer(lambda: db.c.watch(resume_after=inv).close())]
        with db.c.watch(start_after=inv, max_await_time_ms=200) as st:
            out.append([(e["operationType"], e.get("documentKey")) for e in _read(st, 5, 2)])
        return out

    same(run(rust_mongo), run(mongod_rs))
