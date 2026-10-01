"""Does the index key encoder order values the way mongod compares them?

An index answers a range query or a sort by walking its keys in BYTE order, so
`sortkey.encode_value` has to put two values in the same order mongod's
comparison does -- for documents and arrays too, which entry format 4 encodes
by value (formats 1-3 used raw BSON, which leads with a length, so byte order
was SIZE order and an index range over documents scanned the wrong stretch).

Generates random values -- every type an index key can hold, documents and
arrays nested three deep, numbers of all four types with NaN / +-0 / +-Inf --
asks mongod `$cmp` for every pair, and counts the pairs where the sign of the
byte comparison disagrees.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" python tools/probes/value_order_encoding.py [--rust]

`--rust` checks the Rust encoder through `_secantus_core` as well, which must be
built from the same tree. `$cmp` compares WHOLE values, arrays included, which
is the comparison an index key needs; `$sort` would not do (it orders an array
by its smallest or largest element).
"""

from __future__ import annotations

import datetime as dt
import os
import random
import sys
from pathlib import Path
from typing import Any

import bson
import pymongo
from bson import Binary, Decimal128, Int64, MaxKey, MinKey, ObjectId

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "src"))
from secantus import sortkey  # noqa: E402

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SEED = 20260930
N_VALUES = 180
KEYS = ["", "a", "b", "ab", "b0", "é"]


def scalar(rng: random.Random) -> Any:
    kind = rng.randrange(14)
    if kind == 0:
        return rng.choice([0, 1, -1, 2, 7])
    if kind == 1:
        return Int64(rng.choice([0, 1, -3, 2**40]))
    if kind == 2:
        return rng.choice([0.0, -0.0, 1.5, -2.25, 1.0, float("inf"), float("-inf"), float("nan")])
    if kind == 3:
        return Decimal128(rng.choice(["1", "1.0", "-0", "2.5", "NaN", "Infinity"]))
    if kind == 4:
        return rng.choice(["", "a", "b", "ab", "a\x00", "B", "é", "e"])
    if kind == 5:
        return None
    if kind == 6:
        return rng.choice([True, False])
    if kind == 7:
        return dt.datetime(2020, 1, 1) + dt.timedelta(days=rng.randrange(-3, 3))
    if kind == 8:
        return ObjectId(bytes([rng.randrange(3)] * 12))
    if kind == 9:
        return Binary(bytes(rng.randrange(3) for _ in range(rng.randrange(3))))
    if kind == 10:
        return MinKey()
    if kind == 11:
        return MaxKey()
    if kind == 12:
        return rng.choice(["x", "y"])
    return rng.randrange(-2, 3)


def value(rng: random.Random, depth: int = 0) -> Any:
    roll = rng.random()
    if depth < 3 and roll < 0.3:
        return {rng.choice(KEYS): value(rng, depth + 1) for _ in range(rng.randrange(4))}
    if depth < 3 and roll < 0.5:
        return [value(rng, depth + 1) for _ in range(rng.randrange(4))]
    return scalar(rng)


def sign(n: int) -> int:
    return (n > 0) - (n < 0)


def main() -> int:
    rust = "--rust" in sys.argv
    if rust:
        import _secantus_core

    rng = random.Random(SEED)
    values = [value(rng) for _ in range(N_VALUES)]
    # A document with a repeated key is not something a driver sends; drop the
    # collisions the generator made rather than compare them.
    client = pymongo.MongoClient(MONGOD)
    db = client.value_order_probe
    pairs = [(i, j) for i in range(len(values)) for j in range(i + 1, len(values))]
    bad = {"python": 0, "rust": 0}
    shown = 0
    for start in range(0, len(pairs), 2000):
        batch = pairs[start : start + 2000]
        docs = [{"x": {"$literal": values[i]}, "y": {"$literal": values[j]}} for i, j in batch]
        got = db.aggregate(
            [
                {"$documents": docs},
                {"$project": {"_id": 0, "c": {"$cmp": ["$x", "$y"]}}},
            ]
        )
        for (i, j), row in zip(batch, got, strict=True):
            want = row["c"]
            a, b = values[i], values[j]
            ea, eb = sortkey.encode_value(a), sortkey.encode_value(b)
            mine = sign((ea > eb) - (ea < eb))
            if mine != want:
                bad["python"] += 1
                if shown < 12:
                    shown += 1
                    print(f"DIFF python  mongod {want:+d}  ours {mine:+d}\n  {a!r}\n  {b!r}")
            if rust:
                ra = bytes(
                    _secantus_core.sortkey_encode_value(
                        bson.encode({"v": a}), b"\x05\x00\x00\x00\x00"
                    )
                )
                rb = bytes(
                    _secantus_core.sortkey_encode_value(
                        bson.encode({"v": b}), b"\x05\x00\x00\x00\x00"
                    )
                )
                if ra != ea or rb != eb:
                    bad["rust"] += 1
                    if shown < 12:
                        shown += 1
                        print(f"DIFF rust bytes differ from python\n  {a!r}\n  {b!r}")
    client.close()
    print(
        f"=== value order: {len(pairs)} pairs of {len(values)} values -- "
        + ", ".join(f"{k} {v}" for k, v in bad.items() if k == "python" or rust)
        + " divergent ==="
    )
    return 1 if any(bad.values()) else 0


if __name__ == "__main__":
    sys.exit(main())
