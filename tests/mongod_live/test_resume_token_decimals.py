"""Resume tokens in mongod's format whose number is a decimal mongod rebuilds
from a double (RM-075).

mongod's KeyString decoder rebuilds such a decimal with Decimal128
arithmetic -- a directed conversion from the double, a 15-digit one, a
multiply for the tiniest, a continuation added at 34 digits -- and then
rescales its exponent to the six bits `_typeBits` stored. Every token here
puts that number where the token's version goes, so a decimal that decodes
answers 50854 (`wrong type for version`) and one that does not answers the
decoder's own error. Both servers must give the same code and message.
"""

from __future__ import annotations

import random
import struct
from typing import Any

import pytest
from bson import Binary

from tests.backlog.conftest import mongo_answer

PREFIX = "820000000000000001"


def _be(n: int, k: int, invert: bool) -> str:
    b = n.to_bytes(k, "big")
    return (bytes(x ^ 0xFF for x in b) if invert else b).hex().upper()


def _bits(stored_exponent: int) -> Binary:
    """Type bits for one decimal: the numeric type (3), then six bits of
    exponent."""
    return Binary(bytes([0x81, (0x3 | (stored_exponent << 2)) & 0xFF]), 0)


def _double_bits(d: float) -> int:
    return struct.unpack(">Q", struct.pack(">d", d))[0]


# 0.99999999999999988897769753748434595763683319091796875 rounds up to
# 9999999999999998889776975374843460E-34; 10^34 less that is the
# continuation that makes the sum exactly 10^34.
_NEAR_ONE = (_double_bits(0.9999999999999999) << 2) & ((1 << 64) - 1)
_TO_10_34 = 1110223024625156540

CURATED = [
    # The backlog item's own token: a tiny decimal, rescaled to 50830.
    ("2A5FFE000000000005" + "6BC75E2D63100000", 0),
    # A small double with a continuation that overflows 34 digits inexactly
    # (50815), and exactly (the sum is rounded to 10^33 at one exponent up).
    ("2A" + _be(_NEAR_ONE | 1, 8, False) + _be(_TO_10_34 + 1, 8, False), 0),
    ("2A" + _be(_NEAR_ONE | 1, 8, False) + _be(_TO_10_34 + 10, 8, False), 0),
    ("2A" + _be(_NEAR_ONE | 1, 8, False) + _be(_TO_10_34 + 10, 8, False), 30),
    # The same, negative.
    ("28" + _be(_NEAR_ONE | 3, 8, True) + _be(_TO_10_34 + 1, 8, True), 0),
    # Equal to the double, and equal to it rounded up to 15 digits.
    ("2A" + _be(_NEAR_ONE, 8, False), 30),
    ("2A" + _be(_NEAR_ONE | 2, 8, False), 49),
    # A large double: infinity with and without a continuation, and a NaN.
    ("33" + _be(0x7FE0000000000000, 8, False), 0),
    ("33" + _be(0x7FE0000000000001, 8, False) + _be(5, 8, False), 0),
    ("33" + _be(0x7FE0000000000003, 8, False) + _be(5, 8, False), 0),
    # A huge decimal written out: canonical, non-canonical, infinite.
    ("33" + _be((1 << 63) | (7000 << 49), 8, False) + _be(123, 8, False), 7000 & 63),
    ("33" + _be((1 << 63) | (3 << 61), 8, False) + _be(1, 8, False), 0),
    ("33" + _be((1 << 63) | (0x1F << 58), 8, False) + _be(0, 8, False), 0),
    # A fraction after a one-byte and an eight-byte integer part.
    ("2B" + _be(3, 1, False) + _be(0x10000000000003, 7, False) + _be(9, 8, False), 0),
    ("32" + _be((1 << 62) | 1, 8, False) + _be(1, 8, False), 0),
]


def _random_tokens(seed: int, count: int) -> list[tuple[str, int]]:
    """Payloads for every decimal path, at a random stored exponent."""
    rng = random.Random(seed)
    out = []
    for _ in range(count):
        path = rng.choice(["tiny", "small", "large", "huge", "fraction"])
        neg = rng.random() < 0.3
        cont = _be(rng.getrandbits(64), 8, neg)
        if path == "tiny":
            enc = (rng.choice([1, 2]) << 62) | (rng.getrandbits(61) << 1) | 1
            data = ("28" if neg else "2A") + _be(enc, 8, neg) + cont
        elif path == "small":
            enc = (3 << 62) | (rng.getrandbits(60) << 2) | rng.randrange(4)
            data = ("28" if neg else "2A") + _be(enc, 8, neg) + cont
        elif path == "large":
            d = 2.0 ** rng.randrange(63, 1024) * (1 + rng.random())
            enc = ((_double_bits(d) & ~(1 << 62)) << 1) & ((1 << 63) - 1)
            data = ("1F" if neg else "33") + _be(enc | rng.randrange(2), 8, neg) + cont
        elif path == "huge":
            coeff = rng.getrandbits(rng.randrange(1, 114))
            high = (rng.randrange(6195, 12288) << 49) | (coeff >> 64)
            low = coeff & ((1 << 64) - 1)
            data = ("1F" if neg else "33") + _be((1 << 63) | high, 8, neg) + _be(low, 8, neg)
        else:
            n = rng.randrange(1, 9)
            ctype = (40 - n) if neg else (42 + n)
            enc = (rng.getrandbits(8 * n - 1) << 1) | 1
            frac = _be(rng.getrandbits(8 * (8 - n)), 8 - n, neg) if n < 8 else ""
            data = f"{ctype:02X}" + _be(enc, n, neg) + frac + cont
        out.append((data, rng.randrange(64)))
    return out


def _answer(client: Any, data: str, stored_exponent: int) -> tuple:
    c = client["rm075_decimals"]["c"]
    token = {"_data": PREFIX + data + "04", "_typeBits": _bits(stored_exponent)}

    def run() -> Any:
        with c.watch(resume_after=token, max_await_time_ms=10) as s:
            return s.try_next()

    answer = mongo_answer(run)
    return answer if answer[0] == "err" else ("ok",)


@pytest.fixture
def servers(rust_mongo: Any, mongod_rs: Any) -> tuple[Any, Any]:
    for client in (rust_mongo, mongod_rs):
        client["rm075_decimals"]["c"].insert_one({"a": 1})
    return rust_mongo, mongod_rs


def test_curated_decimal_tokens(servers: tuple[Any, Any]) -> None:
    ours, theirs = servers
    codes = set()
    for data, stored in CURATED:
        expected = _answer(theirs, data, stored)
        assert _answer(ours, data, stored) == expected, (data, stored)
        codes.add(expected[1] if expected[0] == "err" else "ok")
    # The curated set reaches the decoder's decimal outcomes, a clean decode
    # (50854) included; if mongod stops answering one, the set needs a look.
    assert {50815, 50829, 50830, 50850, 50854} <= codes, codes


def test_random_decimal_tokens(servers: tuple[Any, Any]) -> None:
    ours, theirs = servers
    diffs = []
    for data, stored in _random_tokens(seed=75, count=400):
        expected = _answer(theirs, data, stored)
        got = _answer(ours, data, stored)
        if got != expected:
            diffs.append((data, stored, got, expected))
    assert not diffs, diffs[:5]
