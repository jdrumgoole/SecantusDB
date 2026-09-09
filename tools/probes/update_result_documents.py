"""Update operators: the DOCUMENT they produce, compared against mongod.

`update_operators.py` next door compares the ERRORS. This compares the
resulting document, which is where a silently wrong WRITE hides -- the update
succeeds, the reply says `nModified: 1`, and the stored document is not the one
mongod would have stored. Nothing else here looks at that.

The seed list is one document per value CLASS, including the four that disagree
with Python's `==` (NaN, signed zero, bool-vs-int, missing-vs-null) and the
array shapes that make a positional path ambiguous. Crossed against every
update operator, including the `$each` / `$sort` / `$slice` modifiers.

A `$currentDate` value is not reproducible, so its TYPE is compared instead --
without that the whole row is noise.

Run it against BOTH servers: `update.py` and `update.rs` are separate ports and
the parity suites pin them to each other, which is satisfied by both being
wrong.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/update_result_documents.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import sys
from pathlib import Path

from bson import Binary, Decimal128, Timestamp

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

SEED = [
    ("scalar", {"v": 1}),
    ("flat", {"v": [1, 2, 3]}),
    ("nested", {"v": [[1, 2]]}),
    ("arrdoc", {"v": [{"k": 1}, {"k": 2}]}),
    ("doc", {"v": {"k": 1}}),
    ("numkey", {"v": {"0": 5}}),
    ("absent", {"w": 1}),
    ("nullv", {"v": None}),
    ("dbl", {"v": 1.5}),
    ("dec", {"v": Decimal128("1.5")}),
    ("nan", {"v": float("nan")}),
    ("negzero", {"v": -0.0}),
    ("boolv", {"v": True}),
    ("strv", {"v": "abc"}),
    ("binv", {"v": Binary(b"z", 0)}),
    ("tsv", {"v": Timestamp(1, 1)}),
    ("emptyarr", {"v": []}),
]
UPDATES = [
    {"$set": {"v": 9}},
    {"$set": {"v.0": 9}},
    {"$set": {"v.k": 9}},
    {"$set": {"v.9": 9}},
    {"$unset": {"v": ""}},
    {"$unset": {"v.0": ""}},
    {"$unset": {"v.k": ""}},
    {"$inc": {"v": 1}},
    {"$inc": {"v.0": 1}},
    {"$mul": {"v": 2}},
    {"$min": {"v": 0}},
    {"$max": {"v": 0}},
    {"$min": {"v": "b"}},
    {"$push": {"v": 4}},
    {"$push": {"v": {"$each": [4, 5]}}},
    {"$push": {"v": {"$each": [4], "$sort": 1}}},
    {"$push": {"v": {"$each": [4], "$slice": 2}}},
    {"$addToSet": {"v": 1}},
    {"$addToSet": {"v": 4}},
    {"$addToSet": {"v": {"$each": [1, 4]}}},
    {"$pop": {"v": 1}},
    {"$pop": {"v": -1}},
    {"$pull": {"v": 1}},
    {"$pull": {"v": {"$gt": 1}}},
    {"$pullAll": {"v": [1, 2]}},
    {"$rename": {"v": "z"}},
    {"$rename": {"v.k": "v.j"}},
    {"$setOnInsert": {"q": 1}},
    {"$bit": {"v": {"and": 1}}},
    {"$bit": {"v": {"or": 4}}},
    {"$currentDate": {"d": {"$type": "timestamp"}}},
]


def _run(client, seed, update):
    db = client["updateresultdocs"]
    db.drop_collection("c")
    db["c"].insert_one({"_id": 1, **seed})
    try:
        db["c"].update_one({"_id": 1}, update)
    except Exception as exc:  # noqa: BLE001 -- the error IS the observation
        detail = getattr(exc, "details", {}) or {}
        return ("ERR", getattr(exc, "code", None), detail.get("errmsg", str(exc))[:90])
    doc = db["c"].find_one({"_id": 1})
    doc.pop("_id", None)
    if "d" in doc:
        # A `$currentDate` value is not reproducible; compare its TYPE.
        doc["d"] = type(doc["d"]).__name__
    return ("OK", repr(doc))


def main() -> int:
    with probe_targets() as (mongod, targets):
        divergent = {label: 0 for label, _ in targets}
        total = 0
        for label, seed in SEED:
            for update in UPDATES:
                total += 1
                want = _run(mongod, seed, update)
                for name, client in targets:
                    got = _run(client, seed, update)
                    if got != want:
                        divergent[name] += 1
                        print(f"<<< [{name}] {label:9} {update}")
                        print(f"    mongod {want}")
                        print(f"    ours   {got}")
        return report("update result documents", total, divergent)


if __name__ == "__main__":
    raise SystemExit(main())
