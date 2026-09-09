"""Decimal128 transcendentals: who is CORRECTLY ROUNDED, us or mongod?

Every other probe here asks "do we match mongod?". This one asks a second
question at the same time — "is the answer right?" — against a 60-digit mpmath
reference, and the pair is what makes the result actionable.

**mongod is not correctly-rounded.** Measured over 285 shapes on 8.2.11
(2026-09-09), it carries Intel RDFP's last-digit error on about 22% of them. So
exact agreement is CAPPED: no choice of working precision reaches it, because
the residual is an implementation artefact we cannot model. That single fact
settles a question this repo had been treating as a policy choice.

What follows from it is the useful part: **being correct is the best available
approximation to mongod.** Where we are correctly-rounded, agreement equals
mongod's own correctness rate; where we are not, we lose twice. That is exactly
how the hyperbolics were caught — computing them at 34 digits "to reproduce
mongod's accumulation" gave `$tanh` 3/20 agreement, and computing wide and
rounding once gave 12/20.

Read the columns together:

* `we refuse` > 0 is the worst cell — a client gets an error where mongod
  returns a number. It was 108/285 on the Rust server until 2026-09-09.
* `agree` well below `m-correct` means OUR accuracy is the problem.
* `agree` tracking `m-correct` means we are correct and the rest is mongod's
  own error — the floor, not a bug.

The exit status reflects only the ACTIONABLE cells: a divergence where we are
also wrong, or a refusal. Differing from mongod while correctly-rounded is the
floor and is reported in the table without failing the run.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run --with mpmath python \
        tools/probes/decimal_transcendental_rounding.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension. Needs `mpmath` for the reference; without it the
probe still reports agreement and says the correctness columns are unavailable.
"""

from __future__ import annotations

import sys
from decimal import Decimal, localcontext
from pathlib import Path

import pymongo
from bson import Decimal128

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

try:
    import mpmath
    from mpmath import mp, mpf

    mp.dps = 60
except ImportError:  # pragma: no cover - the probe still runs, minus a column
    mpmath = None

#: `[-1, 1]`, for the operators whose domain it is.
UNIT = [
    "0.5",
    "-0.5",
    "0.1",
    "0.9",
    "0.25",
    "0.75",
    "0.333",
    "0.987654321",
    "1",
    "-1",
    "0",
    "0.0001",
    "0.6180339887498948482045868343656",
    "-0.123456789",
    "0.999",
]
#: `[1, inf)`, for `$acosh`.
GEQ1 = [
    "1",
    "1.5",
    "2",
    "7",
    "10",
    "42",
    "1.25",
    "123456789.123456789",
    "1.0001",
    "5",
    "2.718281828459045235360287471353",
    "8.125",
    "1000",
    "1.7",
    "3.14159265358979",
]
#: Everything else. Positive throughout so `$ln` / `$log10` / `$sqrt` share it.
ANY = [
    "1.5",
    "2",
    "0.5",
    "0.1",
    "7",
    "10",
    "0.987654321",
    "1.7",
    "0.9",
    "42",
    "0.000001",
    "3.141592653589793238462643383279",
    "123456789.123456789",
    "0.25",
    "1.25",
    "2.718281828459045235360287471353",
    "0.75",
    "5",
    "0.333",
    "8.125",
]


def _ops():
    if mpmath is None:
        return [(n, None, d) for n, _, d in _OPS]
    return _OPS


_OPS = [
    ("$sin", lambda: mpmath.sin, ANY),
    ("$cos", lambda: mpmath.cos, ANY),
    ("$tan", lambda: mpmath.tan, ANY),
    ("$asin", lambda: mpmath.asin, UNIT),
    ("$acos", lambda: mpmath.acos, UNIT),
    ("$atan", lambda: mpmath.atan, ANY),
    ("$sinh", lambda: mpmath.sinh, ANY),
    ("$cosh", lambda: mpmath.cosh, ANY),
    ("$tanh", lambda: mpmath.tanh, ANY),
    ("$acosh", lambda: mpmath.acosh, GEQ1),
    ("$asinh", lambda: mpmath.asinh, ANY),
    ("$ln", lambda: mpmath.log, ANY),
    ("$log10", lambda: lambda x: mpmath.log(x, 10), ANY),
    ("$exp", lambda: mpmath.exp, ANY),
    ("$sqrt", lambda: mpmath.sqrt, ANY),
]


def _truth34(fn, arg):
    """The true value rounded to 34 significant digits, or None."""
    if mpmath is None:
        return None
    try:
        with localcontext() as ctx:
            ctx.prec = 34
            return +Decimal(mp.nstr(fn(mpf(arg)), 34, strip_zeros=False))
    except Exception:  # noqa: BLE001 - a reference failure is not a finding
        return None


def _fetch(client, op, values):
    db = client["dectrans"]
    db.drop_collection("c")
    db["c"].insert_many([{"_id": i, "v": Decimal128(v)} for i, v in enumerate(values)])
    out = {}
    for i in range(len(values)):
        try:
            rows = db["c"].aggregate([{"$match": {"_id": i}}, {"$project": {"r": {op: "$v"}}}])
            rows = list(rows)
            out[i] = rows[0].get("r") if rows else None
        except pymongo.errors.OperationFailure as exc:
            out[i] = ("ERR", exc.code)
    return out


def main() -> int:
    if mpmath is None:
        print("  NOTE: mpmath is absent -- correctness columns unavailable.", file=sys.stderr)
    with probe_targets() as (mongod, targets):
        divergent = {label: 0 for label, _ in targets}
        total = 0
        print(
            f"  {'op':8} {'server':8} {'agree':>6} {'m-correct':>10} "
            f"{'we-correct':>11} {'we refuse':>10}   of"
        )
        for op, mkfn, values in _ops():
            fn = mkfn() if mkfn else None
            want = _fetch(mongod, op, values)
            truths = [_truth34(fn, v) if fn else None for v in values]
            for label, client in targets:
                got = _fetch(client, op, values)
                agree = mc = oc = refuse = 0
                for i, _ in enumerate(values):
                    mv, ov = want.get(i), got.get(i)
                    if isinstance(ov, tuple):
                        refuse += 1
                    matched = mv == ov
                    agree += matched
                    t = truths[i]
                    ours_right = (
                        t is not None
                        and not isinstance(ov, tuple)
                        and ov is not None
                        and ov.to_decimal() == t
                    )
                    if t is not None:
                        mc += not isinstance(mv, tuple) and mv is not None and mv.to_decimal() == t
                        oc += ours_right
                    # Only an ACTIONABLE divergence counts. Differing from mongod
                    # while being correctly-rounded is the FLOOR -- mongod's own
                    # Intel RDFP error, which no working precision reaches -- and
                    # counting it would make this probe fail forever and train
                    # the reader to ignore it. Differing while ALSO being wrong
                    # is ours to fix, and so is any refusal.
                    if not matched and not ours_right:
                        divergent[label] += 1
                    total += 1 if label == targets[0][0] else 0
                print(f"  {op:8} {label:8} {agree:6} {mc:10} {oc:11} {refuse:10}   {len(values)}")
        return report("decimal transcendental rounding", total, divergent)


if __name__ == "__main__":
    raise SystemExit(main())
