"""Differential-probe capped collections against a real mongod.

What a capped collection holds after inserts and upserts, how `create` and
`collMod` read `size` / `max`, what `collStats` reports, and what a write
inside a transaction answers. Run against mongod 8.2.11 on 2026-10-09, the
Rust server differed on 67 of 97 results and the Python server on 51: a batch
insert or an upsert was never held to the cap, and `max: 0` (no limit)
emptied the collection. The Rust server was fixed that day (0 of 97).

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/capped_collections.py

The transaction scenarios need mongod to be a replica-set member; against a
standalone they are reported VACUOUS and not counted.
"""

import contextlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import pymongo  # noqa: E402
from _servers import probe_targets, report  # noqa: E402
from bson import Decimal128, Int64  # noqa: E402
from pymongo.errors import ConfigurationError, PyMongoError  # noqa: E402

NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "recoveryToken")


def typed(v):
    """`v` with every number tagged by its BSON type, and reply noise dropped."""
    if isinstance(v, bool) or v is None or isinstance(v, str):
        return v
    if isinstance(v, Int64):
        return f"long:{int(v)}"
    if isinstance(v, int):
        return f"int:{v}"
    if isinstance(v, float):
        return f"double:{v}"
    if isinstance(v, dict):
        return {k: typed(x) for k, x in v.items() if k not in NOISE}
    if isinstance(v, (list, tuple)):
        return [typed(x) for x in v]
    return repr(v)


def ids(db, name):
    found = [d["_id"] for d in db[name].find()]
    return found if len(found) <= 12 else [len(found), found[0], found[-1]]


def options(db, name):
    return db.command("listCollections", filter={"name": name})["cursor"]["firstBatch"][0][
        "options"
    ]


def fresh(db, name, **opts):
    db.drop_collection(name)
    db.command({"create": name, "capped": True, **opts})
    return db[name]


def docs(lo, hi, payload=1):
    return [{"_id": i, "p": "x" * payload} for i in range(lo, hi)]


def eviction(db):
    """`{name: (create options, [batches])}` -> the ids left."""
    cases = {
        "max3_one_batch": ({"size": 1 << 20, "max": 3}, [docs(0, 5)]),
        "max3_two_batches": ({"size": 1 << 20, "max": 3}, [docs(0, 2), docs(2, 7)]),
        "max1500_one_batch": ({"size": 1 << 24, "max": 1500}, [docs(0, 2500)]),
        "size_and_max": ({"size": 5000, "max": 4}, [docs(0, 30, 300)]),
        "max0": ({"size": 100000, "max": 0}, [docs(0, 5)]),
        "max_negative": ({"size": 100000, "max": -5}, [docs(0, 5)]),
        "duplicate_in_batch": (
            {"size": 1 << 20, "max": 3},
            [[{"_id": 1}, {"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}]],
        ),
    }
    for size in (1000, 4096, 5000, 100000):
        for payload in (100, 700, 3000):
            batch = docs(0, 60, payload)
            cases[f"size{size}_payload{payload}_batch"] = ({"size": size}, [batch])
            cases[f"size{size}_payload{payload}_single"] = ({"size": size}, [[d] for d in batch])
    out = {}
    for name, (opts, batches) in cases.items():
        coll = fresh(db, name, **opts)
        for batch in batches:
            with contextlib.suppress(PyMongoError):
                coll.insert_many(batch, ordered=False)
        out[name] = ids(db, name)
    return out


def upserts(db):
    out = {}
    coll = fresh(db, "up", size=100000, max=3)
    coll.insert_many(docs(0, 3))
    for i in (10, 11, 12):
        coll.update_one({"_id": i}, {"$set": {"a": 1}}, upsert=True)
    out["update_upserts"] = ids(db, "up")
    coll.find_one_and_update({"_id": 77}, {"$set": {"a": 1}}, upsert=True)
    out["findAndModify_upsert"] = ids(db, "up")
    coll.replace_one({"_id": 88}, {"a": 2}, upsert=True)
    out["replace_upsert"] = ids(db, "up")
    coll.bulk_write(
        [pymongo.UpdateOne({"_id": 90 + i}, {"$set": {"a": 1}}, upsert=True) for i in range(4)]
    )
    out["bulk_upserts"] = ids(db, "up")
    coll.update_one({"_id": 93}, {"$set": {"p": "y" * 500}})
    out["update_grow"] = ids(db, "up")
    coll.delete_one({"_id": 93})
    out["delete_one"] = ids(db, "up")
    return out


CREATE = {
    "size_int": {"capped": True, "size": 1000},
    "size_long": {"capped": True, "size": Int64(1000)},
    "size_double": {"capped": True, "size": 1000.0},
    "size_fraction": {"capped": True, "size": 1000.7},
    "size_half": {"capped": True, "size": 0.5},
    "size_zero": {"capped": True, "size": 0},
    "size_negative": {"capped": True, "size": -1},
    "size_1pb": {"capped": True, "size": Int64(2**50)},
    "size_2e62": {"capped": True, "size": Int64(2**62)},
    "size_nan": {"capped": True, "size": float("nan")},
    "size_inf": {"capped": True, "size": float("inf")},
    "size_decimal": {"capped": True, "size": Decimal128("1000")},
    "max_double": {"capped": True, "size": 1000, "max": 3.7},
    "max_zero": {"capped": True, "size": 1000, "max": 0},
    "max_negative": {"capped": True, "size": 1000, "max": -5},
    "max_int_max": {"capped": True, "size": 1000, "max": 2**31 - 1},
    "max_2e31": {"capped": True, "size": 1000, "max": Int64(2**31)},
    "max_nan": {"capped": True, "size": 1000, "max": float("nan")},
    "max_null": {"capped": True, "size": 10, "max": None},
    "capped_one": {"capped": 1, "size": 1000},
    "capped_decimal": {"capped": Decimal128("1"), "size": 10},
    "capped_zero_with_size": {"capped": 0, "size": 1000},
    "no_capped_size_zero": {"size": 0},
    "no_size_max_2e31": {"capped": True, "max": Int64(2**31)},
    "size_null_with_max": {"capped": True, "size": None, "max": 3},
}

COLLMOD = {
    "size": {"cappedSize": 5000},
    "size_fraction": {"cappedSize": 7000.9},
    "size_zero": {"cappedSize": 0},
    "size_2e62": {"cappedSize": Int64(2**62)},
    "size_string": {"cappedSize": "x"},
    "size_null": {"cappedSize": None},
    "max": {"cappedMax": 2},
    "max_zero": {"cappedMax": 0},
    "max_2e31": {"cappedMax": Int64(2**31)},
    "both": {"cappedSize": 9000, "cappedMax": 4},
}


def outcome(fn):
    """What `fn` returned, or the server's refusal of it."""
    try:
        return ("OK", typed(fn()))
    except ConfigurationError:
        raise
    except PyMongoError as exc:
        details = getattr(exc, "details", None) or {}
        return ("ERR", details.get("code"), (details.get("errmsg") or str(exc))[:200])


def option_handling(db):
    out = {}

    def create(name, opts):
        db.drop_collection(name)
        db.command({"create": name, **opts})
        return options(db, name)

    for label, opts in CREATE.items():
        out[f"create_{label}"] = outcome(lambda label=label, opts=opts: create(f"c_{label}", opts))
    fresh(db, "s", size=1000, max=5).insert_many(docs(0, 5))
    fresh(db, "nomax", size=1000)
    fresh(db, "unlimited", size=1000, max=0)
    db.drop_collection("plain")
    db.command({"create": "plain"})
    for name in ("s", "nomax", "unlimited", "plain"):
        stats = db.command("collStats", name)
        kept = {k: stats[k] for k in ("capped", "max", "maxSize") if k in stats}
        out[f"collStats_{name}"] = ("OK", typed(kept))
    for label, opts in COLLMOD.items():
        out[f"collMod_{label}"] = outcome(
            lambda opts=opts: db.command({"collMod": "s", **opts})["ok"]
        )
        out[f"collMod_{label}_options"] = ("OK", typed(options(db, "s")))
    out["collMod_shrink"] = outcome(lambda: db.command({"collMod": "s", "cappedMax": 1})["ok"])
    db.s.insert_one({"_id": 500})
    out["collMod_shrink_then_insert"] = ("OK", typed(ids(db, "s")))
    for label, opts in (("size", {"cappedSize": 5000}), ("size_zero", {"cappedSize": 0})):
        out[f"collMod_not_capped_{label}"] = outcome(
            lambda opts=opts: db.command({"collMod": "plain", **opts})["ok"]
        )
    return out


TXN_WRITES = {
    "insert": {"insert": "tx", "documents": [{"_id": 3}, {"_id": 4}], "ordered": False},
    "update": {"update": "tx", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 2}}}]},
    "upsert": {
        "update": "tx",
        "updates": [{"q": {"_id": 9}, "u": {"$set": {"a": 2}}, "upsert": True}],
    },
    "delete": {"delete": "tx", "deletes": [{"q": {"_id": 1}, "limit": 1}]},
    "findAndModify": {"findAndModify": "tx", "query": {"_id": 1}, "update": {"$set": {"a": 3}}},
    "find": {"find": "tx"},
}


def transactions(db):
    out = {}
    fresh(db, "tx", size=100000).insert_one({"_id": 1, "a": 1})
    for label, cmd in TXN_WRITES.items():
        with db.client.start_session() as session:
            session.start_transaction()
            try:
                reply = dict(db.command(cmd, session=session))
                reply.pop("cursor", None)
            except PyMongoError as exc:
                reply = {"raised": getattr(exc, "code", None), "errmsg": str(exc)[:120]}
            try:
                session.commit_transaction()
                reply["commit"] = "ok"
            except PyMongoError as exc:
                # The wording after a server-side abort is a separate backlog
                # item; the code is what this probe compares.
                reply["commit"] = getattr(exc, "code", None)
        out[f"txn_{label}"] = reply
    out["txn_documents_after"] = list(db.tx.find())
    return out


def run(client, section):
    """One section's results, each `("OK", value)` or `("ERR", code, errmsg)`."""
    db = client["capped_probe"]
    try:
        produced = section(db)
    except ConfigurationError as exc:
        return {section.__name__: ("VACUOUS", str(exc)[:80])}
    return {k: v if isinstance(v, tuple) else ("OK", typed(v)) for k, v in produced.items()}


def main():
    with probe_targets(replica_set="secantus") as (mon, targets):
        divergent = {label: 0 for label, _ in targets}
        total = vacuous = 0
        for section in (eviction, upserts, option_handling, transactions):
            for client in [mon] + [c for _, c in targets]:
                client.drop_database("capped_probe")
            expected = run(mon, section)
            got = {name: run(cli, section) for name, cli in targets}
            for key in sorted(set(expected) | {k for g in got.values() for k in g}):
                want = expected.get(key, ("ABSENT",))
                if want[0] == "VACUOUS":
                    vacuous += 1
                    print(f"  VACUOUS {key}: {want[1]}")
                    continue
                total += 1
                off = {name for name, g in got.items() if g.get(key, ("ABSENT",)) != want}
                if not off:
                    continue
                for name in off:
                    divergent[name] += 1
                print(f"  {key}")
                print(f"    mongod  : {want}")
                for name, g in got.items():
                    mark = "   <-- diverges" if name in off else ""
                    print(f"    {name:8s}: {g.get(key, ('ABSENT',))}{mark}")
        if vacuous:
            print(f"\n  {vacuous} scenario(s) VACUOUS and not counted")
        return report("capped_collections", total, divergent)


if __name__ == "__main__":
    sys.exit(main())
