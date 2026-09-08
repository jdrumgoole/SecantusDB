"""Does mongod compute decimal128 transcendentals via a binary128 intermediate?

Hypothesis, from the `$ln` / `$log10` divergence: mongod converts the decimal128
input to binary128, evaluates there, and converts the result back to 34 decimal
digits -- so its answer differs from the correctly-rounded DECIMAL one.

This computes each function to ~120 digits (series, so no library is needed),
then compares mongod against two candidate rules:

  decimal  -- round the true value to 34 significant decimal digits
  binary128 -- round the true value to a 113-bit significand, then to 34 digits

`$sqrt` is expected to follow the DECIMAL rule: IEEE 754 requires correct
rounding for square root, and does not require it for the transcendentals.
"""

import json
import sys
from decimal import Decimal, getcontext, localcontext
from fractions import Fraction

getcontext().prec = 160
PREC = 130


def _exp(x):
    s, term = Decimal(1), Decimal(1)
    for n in range(1, 200):
        term = term * x / n
        s += term
        if abs(term) < Decimal(10) ** -(PREC + 10):
            break
    return s


def _sin(x):
    s, term = x, x
    for n in range(1, 200):
        term = -term * x * x / ((2 * n) * (2 * n + 1))
        s += term
        if abs(term) < Decimal(10) ** -(PREC + 10):
            break
    return s


def _cos(x):
    s, term = Decimal(1), Decimal(1)
    for n in range(1, 200):
        term = -term * x * x / ((2 * n - 1) * (2 * n))
        s += term
        if abs(term) < Decimal(10) ** -(PREC + 10):
            break
    return s


def _atan(x):
    # |x| > 1 uses atan(x) = pi/2 - atan(1/x) so the series converges.
    if abs(x) > 1:
        return _pi() / 2 * (1 if x > 0 else -1) - _atan(1 / x)
    s, term, xx = x, x, x * x
    for n in range(1, 400):
        term = -term * xx
        s += term / (2 * n + 1)
        if abs(term / (2 * n + 1)) < Decimal(10) ** -(PREC + 10):
            break
    return s


def _pi():
    # Machin: pi = 16*atan(1/5) - 4*atan(1/239)
    def at(inv):
        x = Decimal(1) / inv
        s, term, xx = x, x, x * x
        for n in range(1, 400):
            term = -term * xx
            s += term / (2 * n + 1)
            if abs(term / (2 * n + 1)) < Decimal(10) ** -(PREC + 10):
                break
        return s

    return 16 * at(Decimal(5)) - 4 * at(Decimal(239))


def _ln(x):
    return x.ln()


FUNCS = {
    "$sqrt": lambda x: x.sqrt(),
    "$exp": _exp,
    "$ln": _ln,
    "$log10": lambda x: x.log10(),
    "$sin": _sin,
    "$cos": _cos,
    "$tan": lambda x: _sin(x) / _cos(x),
    "$atan": _atan,
    "$sinh": lambda x: (_exp(x) - _exp(-x)) / 2,
    "$cosh": lambda x: (_exp(x) + _exp(-x)) / 2,
    "$tanh": lambda x: (_exp(2 * x) - 1) / (_exp(2 * x) + 1),
    "$asinh": lambda x: (x + (x * x + 1).sqrt()).ln(),
}


def to_b128(f: Fraction) -> Fraction:
    if f == 0:
        return Fraction(0)
    neg, f = f < 0, abs(f)
    e = f.numerator.bit_length() - f.denominator.bit_length()
    shift = 113 - 1 - e
    n = round(f * Fraction(2) ** shift)
    if n.bit_length() > 113:
        shift -= 1
        n = round(f * Fraction(2) ** shift)
    r = Fraction(n) / Fraction(2) ** shift
    return -r if neg else r


def dec34(v) -> str:
    fr = v if isinstance(v, Fraction) else Fraction(str(v))
    with localcontext() as c:
        c.prec = 34
        return str(+(Decimal(fr.numerator) / Decimal(fr.denominator)))


def norm(s: str) -> str:
    d = Decimal(s)
    return format(d.normalize(), "f")


def main(path):
    with open(path) as fh:
        vals = json.load(fh)
    tally = {}
    for key, want in sorted(vals.items()):
        op, v = key.split("|")
        if want.startswith("ERR") or op not in FUNCS:
            continue
        with localcontext() as c:
            c.prec = PREC
            try:
                true = FUNCS[op](Decimal(v))
            except Exception as exc:
                print(f"  SKIP {key}: {type(exc).__name__}")
                continue
        d_rule = dec34(true)
        b_rule = dec34(to_b128(Fraction(str(true))))
        w = norm(want)
        hit = "decimal" if norm(d_rule) == w else ("binary128" if norm(b_rule) == w else "NEITHER")
        tally.setdefault(op, []).append(hit)
        if hit == "NEITHER":
            print(f"  NEITHER {key}")
            print(f"     mongod  {want}")
            print(f"     decimal {d_rule}")
            print(f"     b128    {b_rule}")
    print("\n--- which rule explains each operator ---")
    for op, hits in sorted(tally.items()):
        n = len(hits)
        d, b, x = hits.count("decimal"), hits.count("binary128"), hits.count("NEITHER")
        verdict = (
            "DECIMAL" if d == n else ("BINARY128" if b == n else f"mixed d={d} b={b} none={x}")
        )
        print(f"  {op:8} {n:2} inputs -> {verdict}")
    allhits = [h for hs in tally.values() for h in hs]
    print(
        f"\ntotal: {len(allhits)} pairs -- decimal {allhits.count('decimal')}, "
        f"binary128 {allhits.count('binary128')}, neither {allhits.count('NEITHER')}"
    )


main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/mvals.json")
