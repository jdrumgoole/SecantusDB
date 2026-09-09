"""`$rename`'s path refusals, against mongod, on both servers.

mongod separates two refusals by WHEN it can decide them, and the two are
reported differently:

* a **dynamic** component (`$`, `$[]`, `$[id]`) in either path is a PARSE
  error -- raised without looking at the document, so an absent source field
  still errors, and sent bare;
* a path that indexes into an **array** is an EXECUTION error -- discovered per
  document, sent under `Plan executor error during update :: caused by ::`, and
  skipped entirely when the source field is absent, because then the `$rename`
  is a no-op.

Precedence: source-dynamic > destination-dynamic > source-array >
destination-array, with the general `No array filter found for identifier`
check ahead of all four.

The SUCCESS cases are as load-bearing as the failures here. The Python server
used to apply `{$rename: {"v.$[].a": "v.$[].b"}}` element-wise, and a test
asserted that it did -- a test written from what the server does, which is what
a probe against the real server is for.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/rename_paths.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

from bson import Decimal128, ObjectId

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

SEED: dict[str, Any] = {
    "_id": 1,
    "v": [{"a": 1}, {"a": 2}],
    "w": {"a": 1},
    "z": 5,
    "deep": {"n": [{"a": 1}]},
    "s": "str",
}

#: `(name, update, kwargs)` -- run against `SEED` unless a case overrides `_id`.
CASES: list[tuple[str, dict, dict]] = [
    # Dynamic components, both sides and both forms.
    ("src-all", {"$rename": {"v.$[].a": "q"}}, {}),
    ("src-dollar", {"$rename": {"v.$.a": "q"}}, {}),
    ("src-identified", {"$rename": {"v.$[e].a": "q"}}, {}),
    ("src-identified-filtered", {"$rename": {"v.$[e].a": "q"}}, {"array_filters": [{"e.a": 1}]}),
    ("dst-all", {"$rename": {"z": "v.$[].b"}}, {}),
    ("dst-dollar", {"$rename": {"z": "v.$.b"}}, {}),
    ("dst-all-nonarray", {"$rename": {"z": "w.$[].b"}}, {}),
    ("both-dynamic", {"$rename": {"v.$[].a": "v.$[].b"}}, {}),
    ("src-dyn-beats-dst-array", {"$rename": {"v.$[].a": "v.0.b"}}, {}),
    ("dst-dyn-beats-src-array", {"$rename": {"v.0.a": "v.$[].b"}}, {}),
    ("dynamic-on-absent-source", {"$rename": {"nope.$[].x": "q"}}, {}),
    ("dynamic-dest-absent-source", {"$rename": {"nope": "v.$[].b"}}, {}),
    # Array elements, source and destination, shallow and deep.
    ("src-index", {"$rename": {"v.0.a": "v.0.b"}}, {}),
    ("src-index-out-of-array", {"$rename": {"v.0.a": "q"}}, {}),
    ("src-index-bare", {"$rename": {"v.0": "q"}}, {}),
    ("src-index-deep", {"$rename": {"deep.n.0.a": "q"}}, {}),
    ("dst-index", {"$rename": {"z": "v.0.b"}}, {}),
    ("dst-index-deep", {"$rename": {"z": "deep.n.0.a"}}, {}),
    # ...and the shapes where the source does not resolve, which are no-ops.
    ("src-index-past-end", {"$rename": {"v.9.a": "q"}}, {}),
    ("src-leaf-missing-under-array", {"$rename": {"v.0.zz": "q"}}, {}),
    ("absent-source-array-dest", {"$rename": {"nope": "v.0.b"}}, {}),
    ("absent-source", {"$rename": {"nope": "q"}}, {}),
    # Neighbours that must keep working.
    ("array-itself", {"$rename": {"v": "u"}}, {}),
    ("plain-nested", {"$rename": {"w.a": "w.b"}}, {}),
    ("numeric-key-on-document", {"$rename": {"w.0": "w.1"}}, {}),
    ("through-a-scalar", {"$rename": {"s.a": "q"}}, {}),
    ("same-path", {"$rename": {"v": "v"}}, {}),
    ("to-non-string", {"$rename": {"z": 5}}, {}),
]

#: The `_id` rendering inside the array-element message is mongod's VALUE repr,
#: not `str()` -- a string `_id` is quoted and an ObjectId is wrapped.
ID_VALUES: list[Any] = [
    1,
    "abc",
    ObjectId("507f1f77bcf86cd799439011"),
    1.5,
    Decimal128("2.5"),
    None,
    True,
]


def _run(client, seed, update, kwargs):
    db = client["renameprobe"]
    db.drop_collection("c")
    db["c"].insert_one(dict(seed))
    try:
        db["c"].update_one({"_id": seed["_id"]}, update, **kwargs)
        return ("OK", db["c"].find_one({"_id": seed["_id"]}))
    except Exception as exc:  # noqa: BLE001 -- the error IS the observation
        return (type(exc).__name__, getattr(exc, "code", None), str(exc).split(", full error")[0])


def _cases():
    for name, update, kwargs in CASES:
        yield name, SEED, update, kwargs
    for value in ID_VALUES:
        seed = {"_id": value, "v": [{"a": 1}]}
        yield f"id-repr-{value!r}", seed, {"$rename": {"v.0.a": "q"}}, {}


def main() -> int:
    with probe_targets() as (mongod, targets):
        divergent = {label: 0 for label, _ in targets}
        total = 0
        for name, seed, update, kwargs in _cases():
            total += 1
            want = _run(mongod, seed, update, kwargs)
            for label, client in targets:
                got = _run(client, seed, update, kwargs)
                if got != want:
                    divergent[label] += 1
                    print(f"<<< [{label}] {name}")
                    print(f"    mongod {want}")
                    print(f"    ours   {got}")
        return report("$rename path refusals", total, divergent)


if __name__ == "__main__":
    raise SystemExit(main())
