#!/usr/bin/env python3
"""Spike 1 driver: pymongo <-> Rust `bson` crate byte-fidelity.

Builds a corpus of BSON documents spanning every type the "pymongo can't tell
us apart" thesis depends on (ObjectId, Decimal128, int32-vs-int64, tz-aware
dates, Binary subtypes, Regex, Timestamp, MinKey/MaxKey, Code, nested docs and
arrays, unicode, numeric boundaries), encodes each with pymongo's `bson`,
streams the concatenated bytes through the Rust `roundtrip` binary, and asserts
the bytes that come back are identical.

A clean run means: for this corpus, the Rust `bson` crate decodes and
re-encodes to the *exact same bytes* pymongo produced. Any per-document diff is
printed with a hexdump so we can see precisely where the crates disagree.

Usage:
    uv run python rust/harness/spike_bson_harness.py path/to/roundtrip
"""
from __future__ import annotations

import datetime
import subprocess
import sys
import uuid

import bson
from bson import (
    Binary,
    Code,
    Decimal128,
    Int64,
    MaxKey,
    MinKey,
    ObjectId,
    Regex,
    Timestamp,
)
from bson.binary import UUID_SUBTYPE


def corpus() -> list[tuple[str, dict]]:
    """Return (label, document) pairs. Labels make diffs legible."""
    tz = datetime.timezone(datetime.timedelta(hours=5, minutes=30))
    docs: list[tuple[str, dict]] = [
        ("empty", {}),
        ("scalars", {"s": "hello", "b_true": True, "b_false": False, "null": None}),
        ("unicode", {"emoji": "\U0001f600é中文", "nul_adjacent": "a\tb\nc"}),
        # int32 vs int64: pymongo picks width by magnitude; Int64 forces 64-bit.
        ("int32_small", {"n": 1, "neg": -1, "zero": 0}),
        ("int32_boundary", {"max32": 2**31 - 1, "min32": -(2**31)}),
        ("int64_forced", {"big": 2**31, "i64": Int64(5), "huge": 2**62}),
        ("doubles", {"pi": 3.141592653589793, "neg": -2.5, "zero": 0.0, "tiny": 5e-324}),
        ("double_specials", {"inf": float("inf"), "ninf": float("-inf"), "nan": float("nan")}),
        ("decimal128", {"d": Decimal128("1.00"), "d2": Decimal128("0"), "big": Decimal128("123456789.987654321")}),
        ("decimal128_specials", {"inf": Decimal128("Infinity"), "nan": Decimal128("NaN"), "neg": Decimal128("-1E-6")}),
        ("objectid", {"_id": ObjectId("0123456789abcdef01234567")}),
        ("datetime_utc", {"t": datetime.datetime(2026, 6, 5, 12, 0, 0, tzinfo=datetime.timezone.utc)}),
        ("datetime_tz", {"t": datetime.datetime(2026, 1, 2, 3, 4, 5, 678000, tzinfo=tz)}),
        ("datetime_epoch", {"t": datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)}),
        ("binary_generic", {"b": Binary(b"\x00\x01\x02\xff", 0)}),
        ("binary_uuid", {"u": Binary(uuid.UUID("12345678-1234-5678-1234-567812345678").bytes, UUID_SUBTYPE)}),
        ("binary_subtype4_func", {"b": Binary(b"raw-bytes-here", 4)}),
        ("regex", {"r": Regex("^abc$", "im"), "r2": Regex(".*", "")}),
        ("timestamp", {"ts": Timestamp(1700000000, 1), "ts0": Timestamp(0, 0)}),
        ("minmax_key", {"lo": MinKey(), "hi": MaxKey()}),
        ("code", {"c": Code("function(){return 1}")}),
        ("nested", {"a": {"b": {"c": [1, 2, {"d": ObjectId()}]}}, "arr": [True, None, "x", 3.5]}),
        ("array_of_mixed", {"xs": [1, Int64(2), 3.0, Decimal128("4"), "5", ObjectId()]}),
        ("key_order", {"z": 1, "a": 2, "m": 3, "_id": 4}),  # insertion order must survive
        ("dotted_like_keys", {"a.b": 1, "$weird": 2}),
    ]
    return docs


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: spike_bson_harness.py <path-to-roundtrip-binary>", file=sys.stderr)
        return 2
    binary = sys.argv[1]

    docs = corpus()
    framed = [bson.encode(d) for _, d in docs]
    stream = b"".join(framed)

    proc = subprocess.run([binary], input=stream, capture_output=True)
    sys.stderr.write(proc.stderr.decode(errors="replace"))
    if proc.returncode not in (0, 1):
        print(f"FAIL: roundtrip binary crashed (exit {proc.returncode})", file=sys.stderr)
        return proc.returncode

    out = proc.stdout
    # Re-split the output by walking the same length prefixes and compare each
    # document independently so a single bad doc doesn't desync the whole stream.
    ok = True
    off_in = off_out = 0
    for (label, _), original in zip(docs, framed):
        got = out[off_out : off_out + len(original)] if off_out < len(out) else b""
        if got == original:
            status = "ok"
        else:
            status = "DIFF"
            ok = False
        print(f"  [{status}] {label} ({len(original)} bytes)")
        if status == "DIFF":
            print(f"      pymongo: {original.hex()}")
            print(f"      rust   : {got.hex()}")
        off_in += len(original)
        off_out += len(original) if got else 0

    if len(out) != len(stream):
        print(f"FAIL: output length {len(out)} != input length {len(stream)}", file=sys.stderr)
        ok = False

    print("\nRESULT:", "PASS — bson crate is byte-faithful to pymongo" if ok else "FAIL — divergence found")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
