"""Compare change-stream events and fatal errors against a real mongod.

**This probe needs a REPLICA SET, not a standalone.** mongod refuses
``$changeStream`` on a standalone ("only supported on replica sets"), which is
why `tests/test_mongod_differential.py` — whose harness spawns a standalone —
has never covered this area, and why it went unprobed until 2026-08-29:

    mongod --replSet rs0 --port 27045 --dbpath /tmp/csrs --fork \
      --logpath /tmp/csrs/log
    mongosh --port 27045 --eval 'rs.initiate()'
    PROBE_MONGOD="mongodb://127.0.0.1:27045" \
      uv run --no-sync python tools/probes/change_streams.py

Set ``PROBE_SERVER`` to a running server's URI to sweep the Rust server instead
of an embedded Python one (see the README).

Values that legitimately differ per run — the opaque resume token, cluster and
wall times, collection UUIDs — are normalised away, so anything printed is a
real behavioural difference.
"""

import os
import re
import tempfile
import time

import pymongo

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27045")
SERVER = os.environ.get("PROBE_SERVER")

# Per-run values: normalised, not compared.
# Per-run values: normalised, not compared. `uuid` is here as well as
# `collectionUUID` because a collection's UUID also appears NESTED, inside
# `stateBeforeChange.collectionOptions` -- normalising only the top-level name
# left that one comparing raw bytes and reported a divergence where the two
# structures were identical.
VOLATILE = (
    "_id",
    "clusterTime",
    "wallTime",
    "collectionUUID",
    "uuid",
    "txnNumber",
    "lsid",
)


def normalise(value):
    if isinstance(value, dict):
        return {k: ("<v>" if k in VOLATILE else normalise(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [normalise(v) for v in value]
    return value


def norm_msg(msg):
    """Strip the resume-token hex and cluster time out of an error message.

    The optional trailing ellipsis in the pattern matters: mongod 8.x builds a
    much longer resume token (it encodes operationType and documentKey), long
    enough that the message is truncated with an ellipsis INSIDE the quotes. A
    pattern requiring quote-hex-quote silently stopped matching there and
    manufactured three divergences that were really one normalisation gap."""
    msg = re.sub(r'"[0-9A-Fa-f]{30,}\.{0,3}"', '"<TOKEN>"', str(msg))
    return re.sub(r"Timestamp\(\d+, \d+\)", "Timestamp(<T>)", msg)


def collect(coll, watch_kwargs, pipeline, writes, *, timeout=5.0):
    """Open a stream, run `writes(coll)`, return the events it produced."""
    events = []
    try:
        with coll.watch(pipeline, **watch_kwargs) as cs:
            assert cs.try_next() is None  # force the aggregate; fixes the start position
            writes(coll)
            deadline = time.time() + timeout
            while time.time() < deadline:
                ev = cs.try_next()
                if ev is not None:
                    events.append(normalise(ev))
                    continue
                if events:
                    break
    except pymongo.errors.PyMongoError as exc:
        details = getattr(exc, "details", None) or {}
        return {
            "error": {
                "code": details.get("code"),
                "codeName": details.get("codeName"),
                "errorLabels": details.get("errorLabels"),
                "errmsg": norm_msg(details.get("errmsg", exc)),
            }
        }
    return {"events": events}


def _set(field, value):
    return lambda c: c.update_one({"_id": 1}, {"$set": {field: value}})


def cases():
    """(name, seed_doc, pipeline, watch_kwargs, writes, pre_images_enabled)."""
    arr = list(range(1, 6))

    # The updateDescription shapes. This is where the 2026-08-29 sweep found
    # mongod NEVER emitting truncatedArrays where we emit it for every shrink.
    yield ("set scalar", {"_id": 1, "a": 1}, [], {}, _set("a", 2), False)
    yield ("set new field", {"_id": 1, "a": 1}, [], {}, _set("b", 2), False)
    yield (
        "unset field",
        {"_id": 1, "a": 1, "b": 2},
        [],
        {},
        lambda c: c.update_one({"_id": 1}, {"$unset": {"b": ""}}),
        False,
    )
    yield ("set array element", {"_id": 1, "arr": arr}, [], {}, _set("arr.2", 99), False)
    yield (
        "push one",
        {"_id": 1, "arr": arr},
        [],
        {},
        lambda c: c.update_one({"_id": 1}, {"$push": {"arr": 9}}),
        False,
    )
    yield (
        "pop last",
        {"_id": 1, "arr": arr},
        [],
        {},
        lambda c: c.update_one({"_id": 1}, {"$pop": {"arr": 1}}),
        False,
    )
    yield (
        "pop first",
        {"_id": 1, "arr": arr},
        [],
        {},
        lambda c: c.update_one({"_id": 1}, {"$pop": {"arr": -1}}),
        False,
    )
    yield (
        "pull middle",
        {"_id": 1, "arr": arr},
        [],
        {},
        lambda c: c.update_one({"_id": 1}, {"$pull": {"arr": 3}}),
        False,
    )
    yield (
        "slice keep 2",
        {"_id": 1, "arr": arr},
        [],
        {},
        lambda c: c.update_one({"_id": 1}, {"$push": {"arr": {"$each": [], "$slice": 2}}}),
        False,
    )
    yield (
        "addToSet",
        {"_id": 1, "arr": arr},
        [],
        {},
        lambda c: c.update_one({"_id": 1}, {"$addToSet": {"arr": 42}}),
        False,
    )
    yield ("replace whole array", {"_id": 1, "arr": arr}, [], {}, _set("arr", [7, 8]), False)
    # Array size matters: mongod's diff switches representation by size.
    for size in (20, 100, 1000):
        big = list(range(size))
        yield (
            f"truncate {size}->2",
            {"_id": 1, "arr": big},
            [],
            {},
            lambda c: c.update_one({"_id": 1}, {"$push": {"arr": {"$each": [], "$slice": 2}}}),
            False,
        )
        yield (f"element in {size}", {"_id": 1, "arr": big}, [], {}, _set("arr.1", -1), False)

    # Other operation types.
    yield (
        "replace doc",
        {"_id": 1, "a": 1},
        [],
        {},
        lambda c: c.replace_one({"_id": 1}, {"b": 2}),
        False,
    )
    yield ("delete", {"_id": 1, "a": 1}, [], {}, lambda c: c.delete_one({"_id": 1}), False)

    # fullDocument / pre-image modes, with and without the collection option.
    for mode in ("updateLookup", "whenAvailable", "required"):
        yield (
            f"fullDocument={mode}",
            {"_id": 1, "a": 1},
            [],
            {"full_document": mode},
            _set("a", 2),
            False,
        )
        yield (
            f"fullDocument={mode} +images",
            {"_id": 1, "a": 1},
            [],
            {"full_document": mode},
            _set("a", 2),
            True,
        )
    for mode in ("whenAvailable", "required"):
        yield (
            f"beforeChange={mode}",
            {"_id": 1, "a": 1},
            [],
            {"full_document_before_change": mode},
            _set("a", 2),
            False,
        )
        yield (
            f"beforeChange={mode} +images",
            {"_id": 1, "a": 1},
            [],
            {"full_document_before_change": mode},
            _set("a", 2),
            True,
        )

    # Fatal: a pipeline that drops or rewrites the resume token.
    yield (
        "project out _id",
        {"_id": 1, "a": 1},
        [{"$project": {"_id": 0}}],
        {},
        _set("a", 2),
        False,
    )
    yield (
        "rewrite _id literal",
        {"_id": 1, "a": 1},
        [{"$project": {"_id": {"$literal": "foo"}}}],
        {},
        _set("a", 2),
        False,
    )
    yield (
        "rewrite _id addFields",
        {"_id": 1, "a": 1},
        [{"$addFields": {"_id": 7}}],
        {},
        _set("a", 2),
        False,
    )
    # A pipeline that keeps the token is fine, including one that drops events.
    yield (
        "match filters events",
        {"_id": 1, "a": 1},
        [{"$match": {"operationType": "insert"}}],
        {},
        _set("a", 2),
        False,
    )
    yield (
        "addFields keeps _id",
        {"_id": 1, "a": 1},
        [{"$addFields": {"extra": 1}}],
        {},
        _set("a", 2),
        False,
    )


# Command events (drop / rename / dropDatabase / index DDL) reach the stream by
# a different construction path from CRUD, so their field order and shape need
# their own cases — the CRUD cases above say nothing about them.
def command_cases():
    """(name, watch_kwargs, action) — each runs against a seeded `c`."""

    def drop(db):
        db.c.drop()

    def rename(db):
        db.c.rename("c2")

    def create_index(db):
        db.c.create_index([("a", 1)], name="probe_ix")

    def drop_index(db):
        db.c.create_index([("a", 1)], name="probe_ix")
        db.c.drop_index("probe_ix")

    def create_coll(db):
        db.create_collection("other")

    def modify(db):
        db.command({"collMod": "c", "changeStreamPreAndPostImages": {"enabled": True}})

    yield ("cmd drop", {}, drop, False)
    yield ("cmd rename", {}, rename, False)
    yield ("cmd createIndexes", {"show_expanded_events": True}, create_index, True)
    yield ("cmd dropIndexes", {"show_expanded_events": True}, drop_index, True)
    yield ("cmd create", {"show_expanded_events": True}, create_coll, True)
    yield ("cmd collMod", {"show_expanded_events": True}, modify, True)
    yield ("crud expanded", {"show_expanded_events": True}, None, True)


def run_command_cases(client, results):
    """Watch the whole DATABASE, so drop/rename/DDL events are in scope."""
    for i, (name, kwargs, action, db_scope) in enumerate(command_cases()):
        dbname = f"probe_cmd_{i}"
        client.drop_database(dbname)
        db = client[dbname]
        db.c.insert_one({"_id": 1, "a": 1})
        events = []
        try:
            with db.watch(**kwargs) as cs:
                assert cs.try_next() is None
                if action is None:
                    db.c.update_one({"_id": 1}, {"$set": {"a": 2}})
                else:
                    action(db)
                deadline = time.time() + 5
                while time.time() < deadline:
                    ev = cs.try_next()
                    if ev is not None:
                        events.append(normalise(ev))
                        continue
                    if events:
                        break
            results[name] = {"events": events}
        except pymongo.errors.PyMongoError as exc:
            details = getattr(exc, "details", None) or {}
            results[name] = {
                "error": {
                    "code": details.get("code"),
                    "codeName": details.get("codeName"),
                    "errmsg": norm_msg(details.get("errmsg", exc)),
                },
                "events": events,
            }
        client.drop_database(dbname)
        _ = db_scope


def run(uri, label):
    results = {}
    client = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10000)
    for i, (name, seed, pipeline, kwargs, writes, images) in enumerate(cases()):
        dbname = f"probe_cs_{i}"
        client.drop_database(dbname)
        db = client[dbname]
        if images:
            db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
        db.c.insert_one(dict(seed))
        try:
            results[name] = collect(db.c, kwargs, pipeline, writes)
        except Exception as exc:  # noqa: BLE001 — a crash IS a result
            results[name] = {"crash": f"{type(exc).__name__}: {norm_msg(exc)}"}
        client.drop_database(dbname)
    run_command_cases(client, results)
    client.close()
    print(f"  {label}: {len(results)} cases")
    return results


def main():
    mine = None
    if SERVER:
        ours = run(SERVER, "server under test")
    else:
        from secantus import SecantusDBServer

        mine = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
        mine.start()
        host, port = mine.address
        ours = run(f"mongodb://{host}:{port}", "SecantusDB")
    try:
        theirs = run(MONGOD, "mongod")
    finally:
        if mine is not None:
            mine.stop()

    diffs = [k for k in ours if ours[k] != theirs.get(k)]
    print(f"\n{len(diffs)} of {len(ours)} cases diverge\n")
    # Every one of them — a capped list silently hides findings.
    for name in diffs:
        print(f"--- {name}")
        print(f"  us : {ours[name]}")
        print(f"  ref: {theirs.get(name)}")

    # Field ORDER is invisible to dict equality, so it gets its own pass —
    # mongod fixes the order of change-event fields and a driver that renders
    # an event verbatim shows ours in a different one.
    order = []
    for name, mine_ in ours.items():
        mine_evs = mine_.get("events", [])
        their_evs = theirs.get(name, {}).get("events", [])
        for a, b in zip(mine_evs, their_evs, strict=False):
            if list(a) != list(b) and (name, list(a), list(b)) not in order:
                order.append((name, list(a), list(b)))
    print(f"\n{len(order)} cases differ in event FIELD ORDER only\n")
    for name, a, b in order:
        print(f"--- {name}\n  us : {a}\n  ref: {b}")


if __name__ == "__main__":
    main()
