"""Number-key format 2 on the Python server's storage (RM-152).

A double is keyed by its EXACT value and the exponent has two bytes, as the
Rust server writes it (``sortkey.NUMBER_KEY_FORMAT``). A store written with
format 1 -- a double keyed by ``Decimal(repr(value))``, every number beyond
exponent +-127 sharing one key per sign -- is re-keyed when it opens: its
``_id`` rows and every index. The format-1 keys here are written by the
encoder as it stood before the change (``_format_1``).
"""

from __future__ import annotations

import math
from decimal import Decimal
from pathlib import Path
from typing import Any

import bson
import pytest
from bson.decimal128 import Decimal128
from bson.int64 import Int64

import secantus.sortkey as sortkey
from secantus import storage as storage_mod
from secantus.storage import Storage


def _format_1(value: Any) -> bytes:
    """``sortkey._encode_number`` as it was before number-key format 2."""
    if isinstance(value, float) and math.isnan(value):
        return b"\x00"
    if isinstance(value, float) and math.isinf(value):
        return b"\xff" if value > 0 else b"\x20"
    if isinstance(value, Decimal128):
        d = value.to_decimal()
        if d.is_nan():
            return b"\x00"
        if d.is_infinite():
            return b"\x20" if d.is_signed() else b"\xff"
    elif isinstance(value, float):
        d = Decimal(repr(value))
    else:
        d = Decimal(int(value))
    if d.is_zero():
        return b"\x80"
    sign = 1 if d > 0 else -1
    d = abs(d).normalize()
    digits, exp = d.as_tuple()[1:3]
    bias_e = 128 + exp + len(digits) - 1
    if not 0 <= bias_e <= 255:
        return bytes([0xC0, 0xFF, 0xFF]) if sign > 0 else bytes([0x40, 0x00, 0x00])
    if sign < 0:
        bias_e = 0xFF - bias_e
    if len(digits) % 2 == 1:
        digits = digits + (0,)
    pairs = bytes(a * 10 + b + 1 for a, b in zip(digits[::2], digits[1::2], strict=True))
    if sign < 0:
        pairs = bytes(0x64 - p for p in pairs)
    prefix = 0xC0 if sign > 0 else 0x40
    return bytes([prefix, bias_e]) + pairs + (b"\x00" if sign > 0 else b"\xff")


def _forget_number_key_format(s: Storage) -> None:
    """Make the store look as a build before format 2 left it."""
    with s._lock:
        c = s._cursor(storage_mod._IDX_TABLE)
        rows = []
        while c.next() == 0:
            key = tuple(c.get_key())
            payload = bson.decode(bytes(c.get_value()))
            payload.get("options", {}).pop(storage_mod._NUMBER_KEYS, None)
            rows.append((key, payload))
        c.reset()
        for key, payload in rows:
            c.set_key(*key)
            c.set_value(bson.encode(payload))
            c.update()
            c.reset()
        meta = s._cursor(storage_mod._OPLOG_META_TABLE)
        meta.set_key(storage_mod._NUMBER_KEYS_META)
        meta.remove()
        meta.reset()


def _us(s: Storage, filt: dict[str, Any]) -> list[int]:
    return sorted(d["u"] for d in s.find_matching("app", "n", filt))


def test_keys_are_exact_and_in_order() -> None:
    enc = sortkey.encode_value
    ascending = [
        -3e250,
        -1.0,
        -1e-300,
        0,
        1e-300,
        Decimal128("0.1"),
        0.1,
        5,
        3e250,
        Decimal128("1E+400"),
    ]
    for low, high in zip(ascending, ascending[1:], strict=False):
        assert enc(low) < enc(high), (low, high)
    assert enc(Int64(2**62)) == enc(2.0**62)
    assert enc(0.1) != enc(Decimal128("0.1"))


def test_a_format_1_store_is_rekeyed_when_it_opens(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = str(tmp_path / "store")
    docs = [
        {"_id": 0.1, "a": 0.1, "u": 1},
        {"_id": Decimal128("0.25"), "a": Decimal128("0.1"), "u": 2},
        {"_id": 1e-300, "a": 1e-300, "u": 3},
        {"_id": "big", "a": 3e250, "u": 4},
        {"_id": -1e-300, "a": -1e-300, "u": 5},
        {"_id": 5, "a": 5, "u": 6},
    ]
    with monkeypatch.context() as m:
        m.setattr(sortkey, "_encode_number", _format_1)
        s = Storage(path, durable=True)
        try:
            n, errors = s.insert("app", "n", [dict(d) for d in docs])
            assert (n, errors) == (len(docs), [])
            s.create_index("app", "n", "a_1", {"a": 1}, {})
            # Format 1 misses 1e-300 below 1 through the index.
            assert 3 not in _us(s, {"a": {"$lt": 1}})
            _forget_number_key_format(s)
        finally:
            s.close()

    s = Storage(path, durable=True)
    try:
        assert s._stored_number_key_format() == sortkey.NUMBER_KEY_FORMAT
        ix = next(i for i in s.list_indexes("app", "n") if i["name"] == "a_1")
        assert ix[storage_mod._NUMBER_KEYS] == sortkey.NUMBER_KEY_FORMAT
        assert _us(s, {}) == [1, 2, 3, 4, 5, 6]
        assert _us(s, {"a": {"$lt": 1}}) == [1, 2, 3, 5]
        assert _us(s, {"a": {"$gt": 1e200}}) == [4]
        for value, u in [(0.1, 1), (Decimal128("0.25"), 2), (1e-300, 3), (-1e-300, 5), (5, 6)]:
            assert _us(s, {"_id": value}) == [u], value
        # A re-keyed `_id` is held.
        _, errors = s.insert("app", "n", [{"_id": 1e-300, "u": 7}])
        assert len(errors) == 1
    finally:
        s.close()
