"""Parity: Rust vs pure-Python Decimal128 arithmetic, generatively.

The curated cases in `test_rust_update_parity.py` / `test_rust_aggregate_parity.py`
pin the behaviours we *knew* to look for. This file is the generator that found
the ones we didn't — during the 2026-08-24 decimal slice it turned up three real
bugs that both review and hand-written cases had missed:

* Rust's float `Display` spells `3.0` as ``"3"`` where Python's `str` spells it
  ``"3.0"`` — the same value with a different **quantum**, so every mixed
  double/decimal result was subtly wrong.
* `add_mag` didn't strip leading zeros, so aligning ``-0E+10`` against
  ``-7.56E-26`` produced 38 leading zeros and rounding — which counts digits
  from the left — kept the zeros and discarded every significant digit.
* An integral double collapsed to ``1E+10`` where mongod and CPython both
  answer ``10000000000``.

None of those are reachable from "sensible" inputs, which is the point: the
generator deliberately emits signed zeros, wide exponent spreads, 34-digit
coefficients, and the double shapes (integral, non-terminating, denormal) where
the two engines' spellings can diverge.

Seeds are fixed, so a failure here reproduces exactly. Two properties are
asserted, not one:

1. **Agreement** — when Rust answers, it must match the pure-Python engine.
2. **No deferral** — for decimal operands the Rust engine must actually
   *answer*. A silent regression to `Fallback` would keep property 1 green
   while breaking the standalone Rust server, which has nowhere to defer to.
"""

from __future__ import annotations

import os
import random

import bson
import pytest
from bson import Int64
from bson.decimal128 import Decimal128

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")

from secantus import aggregate as _agg  # noqa: E402
from secantus import expressions as _expr  # noqa: E402
from secantus import update as _upd  # noqa: E402

# Double shapes whose decimal spelling is where the engines historically parted
# company: exactly-representable, integral-but-large, non-terminating in binary,
# and the denormal edge (where shortest-repr and exact-value rounding differ).
_DOUBLES = [3.0, 1.5, 0.5, 2.25, 0.1, 123.456, 1e10, 1e16, 1e-5, 4.125, 5e-324, 0.0, -0.0]

#: Multiplies every iteration count below. The defaults keep the whole file
#: near a second so it can live in the default suite; crank this when hunting
#: (``SECANTUS_DECIMAL_FUZZ_SCALE=50 pytest -n0 tests/test_rust_decimal_parity.py``).
#: The seeds are fixed either way, so a larger scale is a strict superset.
_SCALE = max(1, int(os.environ.get("SECANTUS_DECIMAL_FUZZ_SCALE", "1")))


def _rand_decimal(rng: random.Random) -> Decimal128:
    """A Decimal128 spanning the coefficient widths and exponents that matter."""
    ndigits = rng.choice([1, 1, 2, 3, 8, 16, 28, 33, 34])
    digits = "".join(rng.choice("0123456789") for _ in range(ndigits))
    digits = digits.lstrip("0") or "0"
    exp = rng.choice([0, 0, -1, -2, -5, -12, -28, -34, 1, 3, 10])
    sign = "-" if rng.random() < 0.35 else ""
    if digits == "0" and rng.random() < 0.5:
        # Signed zeros with a live exponent — the `0E+10` family.
        return Decimal128(f"{sign}0E{'+' if exp >= 0 else ''}{exp}")
    if exp == 0:
        body = digits
    elif exp < 0:
        body = (
            f"{digits[:exp]}.{digits[exp:]}"
            if len(digits) > -exp
            else f"0.{'0' * (-exp - len(digits))}{digits}"
        )
    else:
        body = f"{digits}E+{exp}"
    return Decimal128(f"{sign}{body}")


def _rand_operand(rng: random.Random):
    r = rng.random()
    if r < 0.60:
        return _rand_decimal(rng)
    if r < 0.75:
        return rng.randint(-1000, 1000)
    if r < 0.85:
        return Int64(rng.randint(-(2**40), 2**40))
    return rng.choice(_DOUBLES)


def _rust_update(doc, update):
    return _rust.apply_update(bson.encode(doc), bson.encode(update), False)


def _rust_pipeline(docs, pipeline):
    res = _rust.apply_pipeline(
        bson.encode({"d": list(docs)}),
        bson.encode({"p": list(pipeline)}),
        bson.encode({}),
        bson.encode({}),
    )
    return None if res is None else bson.decode(res)["d"]


def _rust_eval(expr, doc):
    res = _rust.evaluate(bson.encode(doc), bson.encode({"e": expr}), bson.encode({}))
    return None if res is None else bson.decode(res)["r"]


def test_update_operator_decimal_fuzz_parity():
    """`$inc` / `$mul` — the 15-significant-digit double conversion."""
    rng = random.Random(0x0DEC1)
    checked = deferred = 0
    for _ in range(8000 * _SCALE):
        cur, operand = _rand_operand(rng), _rand_operand(rng)
        if not isinstance(cur, Decimal128) and not isinstance(operand, Decimal128):
            continue
        op = rng.choice(["$inc", "$mul"])
        doc = bson.decode(bson.encode({"_id": 1, "v": cur}))
        update = bson.decode(bson.encode({op: {"v": operand}}))

        raw = _rust_update(doc, update)
        if raw is None:
            deferred += 1
            continue
        checked += 1
        assert bson.decode(raw) == _upd.apply_update(dict(doc), update), (
            f"divergence: {op} cur={cur!r} operand={operand!r}"
        )

    assert checked > 2000, f"expected many handled cases, only {checked}"
    # Decimal arithmetic is native now; a deferral means the Rust server, which
    # cannot fall back to Python, would fail the write outright.
    assert deferred == 0, f"Rust deferred {deferred} decimal updates"


def test_accumulator_decimal_fuzz_parity():
    """`$sum` / `$avg` — the *exact* binary double conversion (a different rule)."""
    rng = random.Random(0x0DEC2)
    ctx = _agg.PipelineContext()
    checked = deferred = 0
    for _ in range(2500 * _SCALE):
        vals = [_rand_operand(rng) for _ in range(rng.randint(1, 5))]
        if not any(isinstance(v, Decimal128) for v in vals):
            continue
        acc = rng.choice(["$sum", "$avg"])
        docs = [bson.decode(bson.encode({"_id": i, "x": v})) for i, v in enumerate(vals)]
        pipeline = [{"$group": {"_id": None, "r": {acc: "$x"}}}]

        got = _rust_pipeline(docs, pipeline)
        if got is None:
            deferred += 1
            continue
        checked += 1
        want = _agg.apply_pipeline([dict(d) for d in docs], pipeline, ctx)
        assert got == want, f"divergence: {acc} vals={vals!r}"

    assert checked > 800, f"expected many handled cases, only {checked}"
    assert deferred == 0, f"Rust deferred {deferred} decimal aggregations"


@pytest.mark.parametrize("op", ["$toDecimal", "$convert"])
def test_decimal_conversion_fuzz_parity(op):
    """`$toDecimal` / `$convert` — 15 digits, rounded from the exact value.

    All four implementations (both operators, both servers) were independently
    wrong before this landed, each using shortest-round-trip text.
    """
    rng = random.Random(0x0DEC3)
    checked = 0
    for _ in range(1500 * _SCALE):
        v = rng.choice(_DOUBLES) if rng.random() < 0.6 else _rand_operand(rng)
        doc = bson.decode(bson.encode({"x": v}))
        expr = (
            {"$toDecimal": "$x"}
            if op == "$toDecimal"
            else {"$convert": {"input": "$x", "to": "decimal"}}
        )
        expr = bson.decode(bson.encode({"e": expr}))["e"]

        got = _rust_eval(expr, doc)
        if got is None:
            continue  # strings / exotics still defer; only agreement matters here
        checked += 1
        assert got == _expr.evaluate(expr, doc), f"divergence: {op} on {v!r}"

    assert checked > 500, f"expected many handled cases, only {checked}"
