"""Randomised change-stream events against mongod: what an UPDATE reports.

`change_streams.py` pins 41 hand-written cases. This one generates them: a
random starting document, then a random sequence of writes -- nested `$set` /
`$unset`, `$inc`, `$push` / `$pull` / `$pop` / `$addToSet`, an array shortened
by `$set`, `$rename`, a replacement, an `updateMany`, a delete -- applied to the
same collection on both servers, with a change stream open that asks for
`fullDocument: updateLookup` and `fullDocumentBeforeChange: whenAvailable`.
Every event is compared, `updateDescription` (`updatedFields`,
`removedFields`, `truncatedArrays`, `disambiguatedPaths`) above all, because a
driver applies those deltas to a cached copy and a wrong one corrupts it.

Needs both servers as replica sets (mongod: `--replSet`, initiated). Compares
the Rust server only:

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27058/?directConnection=true" \\
        python tools/probes/change_stream_fuzz.py [N_SCENARIOS]

Per-run values (resume tokens, times, UUIDs) are normalised away.
"""

from __future__ import annotations

import os
import random
import sys
from typing import Any

import pymongo

MONGOD = os.environ.get("PROBE_MONGOD")
SERVER = os.environ.get("PROBE_SERVER")
SEED = int(os.environ.get("PROBE_SEED", "20261001"))
VOLATILE = {"_id", "clusterTime", "wallTime", "collectionUUID", "uuid", "txnNumber", "lsid"}
FIELDS = ["a", "b", "c"]


def normalise(v: Any) -> Any:
    if isinstance(v, dict):
        return {k: ("<v>" if k in VOLATILE else normalise(x)) for k, x in v.items()}
    if isinstance(v, list):
        return [normalise(x) for x in v]
    return v


def rand_value(rng: random.Random, depth: int = 0) -> Any:
    r = rng.random()
    if depth < 2 and r < 0.2:
        return {rng.choice(FIELDS): rand_value(rng, depth + 1) for _ in range(rng.randrange(1, 3))}
    if depth < 2 and r < 0.4:
        return [rand_value(rng, depth + 1) for _ in range(rng.randrange(0, 4))]
    return rng.choice([0, 1, 2, -1, 1.5, "x", "y", None, True])


def rand_path(rng: random.Random) -> str:
    parts = [rng.choice(FIELDS)]
    while rng.random() < 0.35 and len(parts) < 3:
        parts.append(rng.choice([*FIELDS, "0", "1"]))
    return ".".join(parts)


def rand_update(rng: random.Random) -> dict[str, Any] | list[Any]:
    kind = rng.randrange(10)
    if kind == 0:
        return {"$set": {rand_path(rng): rand_value(rng) for _ in range(rng.randrange(1, 3))}}
    if kind == 1:
        return {"$unset": {rand_path(rng): ""}}
    if kind == 2:
        return {"$inc": {rng.choice(FIELDS): rng.choice([1, -2, 0.5])}}
    if kind == 3:
        return {"$push": {rng.choice(FIELDS): rand_value(rng)}}
    if kind == 4:
        return {"$pull": {rng.choice(FIELDS): rand_value(rng)}}
    if kind == 5:
        return {"$pop": {rng.choice(FIELDS): rng.choice([1, -1])}}
    if kind == 6:
        return {"$addToSet": {rng.choice(FIELDS): rand_value(rng)}}
    if kind == 7:
        f = rng.choice(FIELDS)
        return {"$set": {f: [rand_value(rng) for _ in range(rng.randrange(0, 3))]}}
    if kind == 8:
        a, b = rng.sample(FIELDS, 2)
        return {"$rename": {a: b}}
    # pipeline-style update
    return [{"$set": {rng.choice(FIELDS): {"$literal": rand_value(rng)}}}]


def scenario(rng: random.Random) -> list[tuple[str, Any]]:
    ops: list[tuple[str, Any]] = [("insert", {"_id": 1, **{f: rand_value(rng) for f in FIELDS}})]
    ops.append(("insert", {"_id": 2, **{f: rand_value(rng) for f in FIELDS}}))
    for _ in range(rng.randrange(2, 6)):
        r = rng.random()
        if r < 0.7:
            ops.append(("update_one", rand_update(rng)))
        elif r < 0.8:
            ops.append(("update_many", rand_update(rng)))
        elif r < 0.9:
            ops.append(("replace", {f: rand_value(rng) for f in rng.sample(FIELDS, 2)}))
        else:
            ops.append(("delete", None))
    return ops


def run(uri: str, ops: list[tuple[str, Any]]) -> list[Any]:
    client = pymongo.MongoClient(uri)
    db = client.cs_fuzz
    db.drop_collection("c")
    db.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
    coll = db.c
    out: list[Any] = []
    with coll.watch(
        full_document="updateLookup",
        full_document_before_change="whenAvailable",
        max_await_time_ms=300,
    ) as stream:
        for op, arg in ops:
            try:
                if op == "insert":
                    coll.insert_one(arg)
                elif op == "update_one":
                    coll.update_one({"_id": 1}, arg)
                elif op == "update_many":
                    coll.update_many({}, arg)
                elif op == "replace":
                    coll.replace_one({"_id": 1}, arg)
                elif op == "delete":
                    coll.delete_one({"_id": 2})
                out.append(("write", op, "ok"))
            except pymongo.errors.OperationFailure as e:
                out.append(("write", op, e.code))
        # Drain: the stream has seen everything once a getMore comes back empty.
        empty = 0
        while empty < 2:
            ev = stream.try_next()
            if ev is None:
                empty += 1
                continue
            out.append(("event", normalise(ev)))
    client.close()
    return out


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD and PROBE_SERVER are required (see the module docstring)")
        return 2
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 60
    rng = random.Random(SEED)
    bad = 0
    for i in range(n):
        ops = scenario(rng)
        want, got = run(MONGOD, ops), run(SERVER, ops)
        if i == 0 and not any(x[0] == "event" for x in want):
            print("SELF-CHECK FAILED: mongod produced no events")
            return 2
        if want != got:
            bad += 1
            if bad <= 6:
                print(f"--- scenario {i}: {ops}")
                for j, (w, g) in enumerate(zip(want, got, strict=False)):
                    if w != g:
                        print(f"  first difference at item {j}\n  mongod: {w}\n  ours:   {g}")
                        break
                else:
                    print(f"  length differs: mongod {len(want)} ours {len(got)}")
    print(f"=== change-stream fuzz: {bad} of {n} scenarios divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
