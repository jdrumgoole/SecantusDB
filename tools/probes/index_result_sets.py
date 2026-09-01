"""Does an INDEX change the answer? Compare the server against ITSELF.

Every other probe here compares SecantusDB to mongod. This one primarily
compares SecantusDB to itself — the same query, the same documents, run once on
a collection carrying the index and once on one without it. If those disagree,
the index changed the answer, which is the bug class this exists for and the one
no other check catches. Three of the four defects it found on 2026-09-01 were
silent data loss: the query succeeded, the shape was right, and rows were simply
missing.

**Why self-comparison rather than mongod for the ORDER.** An earlier version
compared ordered `_id` lists against mongod and reported 58 of 1692
"divergences" that were all tie-order under equal sort keys — which mongod does
not promise, and does not itself reproduce run to run. Switching to sort-key
VALUES cut that to 8, and those 8 turned out to be array-sort and cross-type
ordering: real, pre-existing, and nothing to do with indexes. Against mongod
this probe therefore compares the `_id` SET only. Ordering belongs to the sort
engine, and mixing it in here buries the finding this probe is for.

Run it against BOTH servers. The Python and Rust servers have separate storage
layers with separate ports of the same helpers, and the engine-parity suites do
NOT cover storage — they pin `query` / `update` / `expressions` / `projection` /
`sortkey` / `diff` / `aggregate`. A storage-layer divergence between the two is
caught by nothing else, and that is exactly what happened: the Python side was
fixed first and the Rust side still had all four.

    # Python server (an embedded one is started when PROBE_SERVER is unset)
    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python tools/probes/index_result_sets.py

    # Rust server
    crates/secantusdb/target/debug/secantusd-rs --port 27055 --storage-path /tmp/rs &
    PROBE_SERVER="mongodb://127.0.0.1:27055" \
      PROBE_MONGOD="mongodb://127.0.0.1:27041" \
      uv run python tools/probes/index_result_sets.py

The CURATED block runs the exact shapes behind the four fixes; the randomised
block is what found them. Keep both: the curated one says which bug came back,
the random one finds the next.
"""

import os
import random
import sys
import tempfile

import pymongo

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")

#: The four defects, as the smallest shapes that expose them.
CURATED = [
    (
        "sparse index vs a null-equality query",
        [("a", 1)],
        {"sparse": True},
        [{"_id": 1, "a": None, "b": 1}, {"_id": 2, "b": 1}, {"_id": 3, "a": 5, "b": 1}],
        [
            {"a": None},
            {"a": {"$eq": None}},
            {"a": {"$in": [None, 5]}},
            # A RANGE bound against null matches an absent field too — the blind
            # spot in the first version of the gate.
            {"a": {"$lte": None}},
            {"a": {"$gte": None}},
            {"a": 5},
        ],
        None,
    ),
    (
        "sparse index walked for a SORT",
        [("a", 1)],
        {"sparse": True},
        [{"_id": 1, "a": None}, {"_id": 2}, {"_id": 3, "a": 5}],
        [{}],
        {"a": 1},
    ),
    (
        "compound sparse index, doc missing one field",
        [("a", 1), ("b", 1)],
        {"sparse": True},
        [
            {"_id": 1, "a": 1, "b": 1},
            {"_id": 2, "a": 1},
            {"_id": 3, "b": 1},
            {"_id": 4, "a": 1, "b": None},
            {"_id": 5},
        ],
        [{"a": 1}, {"a": {"$gte": 0}}, {"a": 1, "b": None}],
        None,
    ),
    (
        "partial-filter implication ACROSS type brackets",
        [("a", 1)],
        {"partialFilterExpression": {"b": {"$gt": 0}}},
        [{"_id": 1, "a": 5, "b": "x"}, {"_id": 2, "a": 5, "b": 7}],
        [{"a": 5, "b": "x"}, {"a": 5, "b": 7}],
        None,
    ),
    (
        "query naming only the partial filter's own fields",
        [("a", 1)],
        {"partialFilterExpression": {"b": {"$gt": 0}}},
        [{"_id": 1, "a": 1, "b": 5}, {"_id": 2, "a": 2, "b": 0}, {"_id": 3, "b": 9}],
        [{"b": 5}, {"b": 9}, {"b": 0}],
        None,
    ),
]

VALUES = [None, 0, 1, 5, "x", "y", [1, 2], [], {"k": 1}, True]
INDEXES = [
    ([("a", 1)], {"sparse": True}),
    ([("a", 1)], {}),
    ([("a", 1), ("b", 1)], {"sparse": True}),
    ([("a", 1), ("b", 1)], {}),
    ([("b", -1)], {"sparse": True}),
    ([("a", 1)], {"partialFilterExpression": {"b": {"$gt": 0}}}),
    ([("a", 1)], {"partialFilterExpression": {"b": {"$lte": 1.5}}}),
]


def _ids(coll, query, sort):
    """The ``_id`` SET a query returns. Order is deliberately not part of it —
    see the module docstring."""
    order = list(sort.items()) if sort else None
    return sorted(d["_id"] for d in coll.find(query, sort=order))


def main() -> int:
    mon = pymongo.MongoClient(MONGOD)
    srv = None
    if SERVER:
        sec = pymongo.MongoClient(SERVER)
    else:
        from secantus import SecantusDBServer

        srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
        srv.start()
        sec = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}")

    bad = total = 0
    for i, (name, keys, opts, docs, queries, sort) in enumerate(CURATED):
        dbn = f"probe_idx_curated{i}"
        mon.drop_database(dbn)
        sec.drop_database(dbn)
        # `c` carries the index; `bare` is the same data with none. The
        # difference between them IS the bug.
        for db in (mon[dbn], sec[dbn]):
            db.c.insert_many([dict(d) for d in docs])
            db.bare.insert_many([dict(d) for d in docs])
            db.c.create_index(keys, name="ix", **opts)
        for q in queries:
            total += 1
            indexed = _ids(sec[dbn].c, q, sort)
            bare = _ids(sec[dbn].bare, q, sort)
            expected = _ids(mon[dbn].c, q, sort)
            if indexed != bare or indexed != expected:
                bad += 1
                print(f"DIFF [{name}] q={q} sort={sort}")
                print(f"  indexed={indexed}  no-index={bare}  mongod={expected}")
    print(f"--- curated: {bad} of {total} divergent")

    rng = random.Random(20260901)
    gen = lambda: rng.choice(  # noqa: E731
        [
            lambda f: {f: rng.choice(VALUES)},
            lambda f: {f: {"$eq": rng.choice(VALUES)}},
            lambda f: {f: {"$in": rng.sample(VALUES, 2)}},
            lambda f: {f: {"$gt": rng.choice([0, 1, 5])}},
            lambda f: {f: {"$lte": rng.choice([0, 1, 5, 1.5])}},
            lambda f: {f: {"$ne": rng.choice(VALUES)}},
            lambda f: {f: {"$exists": rng.choice([True, False])}},
        ]
    )
    rbad = rtotal = 0
    for trial in range(150):
        dbn = f"probe_idx_fuzz{trial}"
        mon.drop_database(dbn)
        sec.drop_database(dbn)
        docs = []
        for i in range(14):
            d = {"_id": i}
            if rng.random() < 0.75:
                d["a"] = rng.choice(VALUES)
            if rng.random() < 0.75:
                d["b"] = rng.choice(VALUES)
            docs.append(d)
        keys, opts = INDEXES[trial % len(INDEXES)]
        try:
            for db in (mon[dbn], sec[dbn]):
                db.c.insert_many([dict(d) for d in docs])
                db.bare.insert_many([dict(d) for d in docs])
                db.c.create_index(keys, name="ix", **opts)
        except pymongo.errors.PyMongoError:
            continue  # mongod rejects the combination (e.g. parallel arrays)
        for _ in range(12):
            q = gen()(rng.choice(["a", "b"]))
            if rng.random() < 0.3:
                q.update(gen()(rng.choice(["a", "b"])))
            sort = rng.choice([None, {"a": 1}, {"b": -1}])
            rtotal += 1
            try:
                indexed = _ids(sec[dbn].c, q, sort)
                bare = _ids(sec[dbn].bare, q, sort)
                expected = _ids(mon[dbn].c, q, sort)
            except pymongo.errors.PyMongoError:
                continue
            if indexed != bare or indexed != expected:
                rbad += 1
                if rbad <= 6:
                    print(f"DIFF idx={keys}{opts} q={q} sort={sort}")
                    print(f"  indexed={indexed}  no-index={bare}  mongod={expected}")
                    print(f"  docs={docs}")
    print(f"--- randomised: {rbad} of {rtotal} divergent")

    if srv is not None:
        srv.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
