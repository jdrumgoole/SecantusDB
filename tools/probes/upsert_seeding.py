"""Upsert: the document mongod SEEDS from the query, plus the update applied.

When an upsert finds no match, mongod builds the new document from the QUERY
before applying the update — and it reads more than bare equality. Getting that
wrong is a silently wrong INSERT: the command succeeds, `upserted` names an
`_id`, and the stored document is missing a field mongod would have written.
The `update_result_documents` probe cannot see it, because it only ever
UPDATES an existing document.

The rule is "clauses that imply a single equality", so the query list crosses
the forms that DO imply one (`$eq`, a one-element `$in` / `$all`, `$and`, a
one-branch `$or`) against the forms that deliberately do not (a longer `$in`,
`$ne`, `$exists`, `$type`, `$elemMatch`, a range). Both halves matter: seeding
too much is as wrong as seeding too little.

A generated `_id` is not reproducible, so its TYPE is compared instead — an
explicit `_id: 5` in the query still compares by value, which is the case that
proves the query reached the seed at all.

**Field ORDER is deliberately not compared here**, and this is the one probe in
the directory where that is right. mongod emits the seeded fields in its own
hash order — query `{a: 1, b: 2}` gives `b, a` — which is neither the query's
order nor sorted, and which CLAUDE.md records as having CHANGED between 6.0.16
and newer servers. Both servers emit them sorted, deliberately. Comparing order
would make this probe report four divergences forever and train the next reader
to ignore it; the field/value PAIRS are what carry the meaning, so that is what
is compared. (Note the contrast with `sort_path_resolution.py`, where order IS
the subject.)

Run it against BOTH servers; the seeding logic is a separate port on each side.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/upsert_seeding.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import sys
from pathlib import Path

from bson import Decimal128

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

QUERIES = [
    # Forms that imply a single equality, so mongod seeds from them.
    {"a": 1},
    {"a.b": 1},
    {"a.b.c": 1},
    {"a": {"$eq": 1}},
    {"a": {"$in": [1]}},
    {"a": {"$all": [1]}},
    {"$and": [{"a": 1}, {"b": 2}]},
    {"$or": [{"a": 1}]},
    {"a": 1, "b": 2},
    {"a": None},
    {"a": [1, 2]},
    {"a": {"b": 1}},
    {"a": Decimal128("1.5")},
    {"_id": 5, "a": 1},
    # ...and forms that do not: mongod seeds nothing from these.
    {"a": {"$gt": 1}},
    {"a": {"$in": [1, 2]}},
    {"a": {"$exists": True}},
    {"a": {"$ne": 1}},
    {"a": {"$type": "int"}},
    {"a": {"$elemMatch": {"b": 1}}},
]

UPDATES = [
    {"$set": {"z": 9}},
    {"$inc": {"n": 1}},
    {"$push": {"arr": 1}},
    {"$setOnInsert": {"s": 1}},
    # These two touch the same path the query seeds, which is where an
    # "is it matched twice?" error would surface.
    {"$set": {"a": 7}},
    {"$unset": {"a": ""}},
]


def _run(client, query, update):
    db = client["upsertseeding"]
    db.drop_collection("c")
    try:
        db["c"].update_one(query, update, upsert=True)
    except Exception as exc:  # noqa: BLE001 -- the error IS the observation
        detail = getattr(exc, "details", {}) or {}
        return ("ERR", getattr(exc, "code", None), detail.get("errmsg", str(exc))[:90])
    docs = list(db["c"].find({}))
    normalised = []
    for doc in docs:
        # A generated `_id` is not reproducible; keep its TYPE.
        if not isinstance(doc.get("_id"), int):
            doc["_id"] = type(doc["_id"]).__name__
        # Sorted PAIRS, not the document's key sequence -- see the module
        # docstring on why field order is out of scope here.
        normalised.append(sorted(doc.items(), key=lambda kv: kv[0]))
    return ("OK", repr(normalised))


def main() -> int:
    with probe_targets() as (mongod, targets):
        divergent = {label: 0 for label, _ in targets}
        total = 0
        for query in QUERIES:
            for update in UPDATES:
                total += 1
                want = _run(mongod, query, update)
                for label, client in targets:
                    got = _run(client, query, update)
                    if got != want:
                        divergent[label] += 1
                        print(f"<<< [{label}] {query}  +  {update}")
                        print(f"    mongod {want}")
                        print(f"    ours   {got}")
        return report("upsert seeding", total, divergent)


if __name__ == "__main__":
    raise SystemExit(main())
