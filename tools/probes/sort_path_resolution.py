"""How a SORT resolves a dotted path -- against mongod, on both servers.

Two questions, both about the same walk and neither answerable from a filter:

**Which value does the path resolve to?** mongod's sort-key generation walks a
dotted path THROUGH arrays, and descends one level into an array-valued key
reached by a FIELD NAME but not into one reached by an INDEX. So
``{x: [[5]]}`` sorted by ``x.0`` ranks among the ARRAYS -- its key is ``[5]`` --
while ``{x: [{y: [1, 2]}]}`` sorted by ``x.y`` ranks by ``1``. Both servers
descended in every case, ranking the first document as the NUMBER 5: wrong
order, and wrong RESULTS as soon as a ``limit`` is involved. Neither the
parity suites nor the index probe could see it -- parity pinned the two engines
to each other and they were wrong together, and ``index_result_sets`` compares
``_id`` SETS precisely so ordering stays out of it.

**When does mongod refuse the path outright?** A component that is a valid
INDEX of the array *and* also a key of some element document is ambiguous, and
mongod answers ``16746`` rather than choosing a reading. Both halves are
load-bearing: ``x.1`` over ``[{"1": 5}]`` is fine (index 1 is past the end) and
``x.0`` over ``[{"00": 5}]`` is fine (``"00"`` is not the key ``"0"``). The
element carrying the key need not be the one at that index.

Ordering is compared here, unlike in ``index_result_sets``, because ordering is
the whole subject -- so every sort carries ``_id`` as a tie-break and no case
leaves two documents with equal keys.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/sort_path_resolution.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

from bson import Decimal128

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

# --- resolution: where does the path land, and is it descended? -------------
#
# Each case ranks a probe document against sentinels whose value AT THE SAME
# PATH is a number, a string and an array -- so the probe's position names the
# BSON type bracket its sort key landed in.
RESOLUTION: list[tuple[Any, list[Any], str]] = [
    ([[5]], [[6], ["zz"], [[4]]], "x.0"),
    ([{"y": [1, 2]}], [{"y": 6}, {"y": "zz"}, {"y": [4]}], "x.y"),
    ([[5], [3]], [[6], ["zz"], [[4]]], "x.0"),
    ({"y": [1, 2]}, [{"y": 6}, {"y": "zz"}, {"y": [4]}], "x.y"),
    ([[5]], [6, "zz", [4]], "x"),
    ([{"y": [[7]]}], [{"y": 6}, {"y": "zz"}, {"y": [4]}], "x.y"),
    ([1, [2]], [[9, 6], [9, "zz"], [9, [4]]], "x.1"),
    ([{"y": 5}, {"y": 1}], [{"y": 6}, {"y": "zz"}, {"y": [4]}], "x.y"),
    ([], [6, "zz", [4]], "x"),
    ([{"y": None}], [{"y": 6}, {"y": "zz"}, {"y": [4]}], "x.y"),
    ([{"y": []}], [{"y": 6}, {"y": "zz"}, {"y": [4]}], "x.y"),
    ([{"y": 5}], [{"y": 6}, {"y": "zz"}, {"y": [4]}], "x.y.0"),
]

# --- ambiguity: which numeric components does mongod refuse? ----------------
AMBIGUITY_ARRAYS: list[list[Any]] = [
    [{"0": 5}],
    [{"1": 5}],
    [{"2": 5}],
    [{"00": 5}],
    [{"-1": 5}],
    [{"0": 5}, {"1": 6}],
    [{"a": 5}, {"0": 6}],
    [{"1": 5}, {"a": 1}],
    [{"2": 5}, {"a": 1}, {"b": 2}],
    [5, {"0": 1}],
    [5, 6],
    [[5]],
    [],
    [[{"0": 5}]],
    [{"y": [{"0": 5}]}],
    [{"0": Decimal128("1.5")}],
    [{"0": "s"}, {"a": [1, 2]}],
]
AMBIGUITY_KEYS = ["x.0", "x.1", "x.2", "x.0.0", "x.y.0", "x"]


def _run(client, docs, key, direction, agg):
    db = client["sortpathprobe"]
    db.drop_collection("c")
    db["c"].insert_many(docs)
    spec = [(key, direction), ("_id", 1)]
    try:
        if agg:
            rows = list(
                db["c"].aggregate(
                    [{"$sort": dict(spec)}, {"$project": {"_id": 1}}], allowDiskUse=False
                )
            )
        else:
            rows = list(db["c"].find({}, {"_id": 1}).sort(spec))
        return [r["_id"] for r in rows]
    except Exception as exc:  # noqa: BLE001 -- the error IS the observation
        code = getattr(exc, "code", None)
        return f"{type(exc).__name__}({code}): {getattr(exc, 'details', {}).get('errmsg', exc)}"


def _cases():
    for probe, sentinels, key in RESOLUTION:
        docs = [{"_id": 0, "x": probe}]
        docs += [{"_id": i + 1, "x": s} for i, s in enumerate(sentinels)]
        for agg in (False, True):
            for direction in (1, -1):
                yield f"resolve {key} x={probe!r}", docs, key, direction, agg
    for arr in AMBIGUITY_ARRAYS:
        docs = [{"_id": 1, "x": arr}, {"_id": 2, "x": [9]}, {"_id": 3}]
        for key in AMBIGUITY_KEYS:
            for agg in (False, True):
                yield f"ambig {key} x={arr!r}", docs, key, 1, agg


def main() -> int:
    with probe_targets() as (mongod, targets):
        divergent = {label: 0 for label, _ in targets}
        total = 0
        for name, docs, key, direction, agg in _cases():
            total += 1
            want = _run(mongod, docs, key, direction, agg)
            for label, client in targets:
                got = _run(client, docs, key, direction, agg)
                if got != want:
                    divergent[label] += 1
                    lane = "agg" if agg else "find"
                    order = "asc" if direction == 1 else "desc"
                    print(f"<<< [{label}] {lane} {order} {name}")
                    print(f"    mongod {want}")
                    print(f"    ours   {got}")
        return report("sort path resolution", total, divergent)


if __name__ == "__main__":
    raise SystemExit(main())
