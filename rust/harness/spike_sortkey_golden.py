#!/usr/bin/env python3
"""Spike 3 generator: golden sort-key vectors from secantus.sortkey.

For each test value we build ``{"v": value}``, BSON round-trip it (so the value
the Rust side decodes is exactly the value we key), compute the authoritative
sort key with the *production* ``secantus.sortkey.encode_value``, and emit a
record ``{"label", "v": <value>, "k": Binary(<keybytes>)}``. The records are
written as a framed BSON stream to the path given on argv.

The Rust `goldencheck` binary reads the same stream, recomputes the key for
each ``v`` with its own port of the encoder, and asserts byte-equality against
``k``. This pins the byte-exact contract for the riskiest encoder — the
"lexical decimal" numeric form where int/long/double/Decimal128 must collide on
equal value and order correctly across the unified numeric type.

Usage:
    PYTHONPATH=src uv run python rust/harness/spike_sortkey_golden.py out.bson
"""
from __future__ import annotations

import datetime
import sys

import importlib.util
import pathlib

import bson
from bson import Binary, Decimal128, Int64, MaxKey, MinKey, ObjectId
from bson.timestamp import Timestamp

# Load secantus.sortkey directly by file path: the package __init__ eagerly
# imports the full server (WiredTiger C extension, shapely, s2sphere), which a
# byte-level encoder spike has no business pulling in. sortkey.py's only
# top-level dependency is bson.
_sortkey_path = pathlib.Path(__file__).resolve().parents[2] / "src" / "secantus" / "sortkey.py"
_spec = importlib.util.spec_from_file_location("secantus_sortkey", _sortkey_path)
_sortkey = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_sortkey)
encode_value = _sortkey.encode_value


def values() -> list[tuple[str, object]]:
    return [
        ("null", None),
        ("minkey", MinKey()),
        ("maxkey", MaxKey()),
        ("bool_t", True),
        ("bool_f", False),
        # Numbers — the hard path. int32/int64/double/Decimal128 of equal
        # value must produce identical key bytes.
        ("int_zero", 0),
        ("int_one", 1),
        ("int_neg_one", -1),
        ("int_1000", 1000),
        ("int_120", 120),
        ("int_max32", 2**31 - 1),
        ("int_min32", -(2**31)),
        ("int64_big", Int64(2**40)),
        ("int64_neg_big", Int64(-(2**40))),
        ("double_1_5", 1.5),
        ("double_neg_2_5", -2.5),
        ("double_pi", 3.141592653589793),
        ("double_123_45", 123.45),
        ("dec_1_00", Decimal128("1.00")),       # equals int 1
        ("dec_one", Decimal128("1")),
        ("dec_123_45", Decimal128("123.45")),    # equals double 123.45
        ("dec_neg_1e6", Decimal128("-1E-6")),
        ("cross_int_vs_double_3", 3),
        ("cross_double_3", 3.0),
        ("cross_dec_3", Decimal128("3")),
        ("string_empty", ""),
        ("string_ascii", "hello"),
        ("string_unicode", "café\U0001f600"),
        ("string_with_nul", "a\x00b"),
        ("objectid", ObjectId("0123456789abcdef01234567")),
        ("date_utc", datetime.datetime(2026, 6, 5, 12, 0, 0, tzinfo=datetime.timezone.utc)),
        ("date_epoch", datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)),
        ("date_pre_epoch", datetime.datetime(1960, 1, 1, tzinfo=datetime.timezone.utc)),
        ("timestamp", Timestamp(1700000000, 7)),
        ("binary", Binary(b"\x00\x01\xff\x00ab", 0)),
    ]


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: spike_sortkey_golden.py <out.bson>", file=sys.stderr)
        return 2
    out_path = sys.argv[1]

    records: list[bytes] = []
    for label, value in values():
        # Round-trip the value through BSON so the typed value we key is
        # exactly the one the Rust side will decode.
        v = bson.decode(bson.encode({"v": value}))["v"]
        key = encode_value(v)
        rec = {"label": label, "v": v, "k": Binary(key, 0)}
        records.append(bson.encode(rec))

    with open(out_path, "wb") as fh:
        fh.write(b"".join(records))
    print(f"wrote {len(records)} golden vectors to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
