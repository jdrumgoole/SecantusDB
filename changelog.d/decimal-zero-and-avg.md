### `$avg` gave a wrong number, and a decimal zero got the wrong quantum

Two families that both looked like "needs 34-digit decimal math" and were not.

**`$avg` was a wrong answer on the Python server.** mongod converts the integer
total to a double and *then* divides — it does not do exact integer division.
The two agree until the total passes 2⁵³, and then they do not:

```
$avg: [2**53+1, 2**53+3, 2**53+5]
    mongod  9007199254740994.0     (float(sum) / n)
    before  9007199254740996.0     (sum / n, correctly rounded)
```

Python's `int / int` is correctly rounded over the exact quotient — a *better*
answer, and the wrong one, because the conformance target is mongod's
arithmetic rather than the most accurate arithmetic. The Rust engine deferred
above 2⁵³ with a comment reading "defer to Python int/int divide", so it was
deferring **to that wrong answer** — a comment justifying behaviour by the
other engine instead of by the oracle.

**A decimal zero answers a constant, and the quantum is load-bearing.** No
series has to run, which is exactly what separates these from the finite
decimals in the same operators. Both the per-operator quantum and the sign rule
are unguessable:

| | mongod | Rust before | Python before |
| --- | --- | --- | --- |
| `$tan(0)` | `0E-40` | *deferred* | `0` |
| `$asinh(0)` | `0E-6176` | *deferred* | `0` |
| `$cos(0)` | `1.000000000000000000000000000000000` | *deferred* | `1` |
| `$sin(-0)` | `-0` | *deferred* | `0` — sign lost |
| `$degreesToRadians(0)` | `0E-35` | *deferred* | `0E-50` |

The odd functions carry `-0` through and the even ones drop it. Every cell was
generated from mongod 8.2.11 rather than derived.

Triaging the family by measurement rather than by its label is what found this:
of the 35 shapes where mongod answered and the Rust engine deferred, **19**
genuinely needed decimal transcendentals and **16 did not**. Rust refusals in
this corpus go 35 → 19, and the remaining 19 are now exclusively the finite
transcendentals — the one genuine dependency question.

#### Fixed

- `secantus.expressions`: `$avg` divides the way mongod does; the trig,
  hyperbolic and angle-conversion operators answer a decimal zero from a
  measured table instead of running their series.
- `secantus-core`: the same table, plus `$sqrt` and `$exp` at zero; the `$avg`
  precision guard is gone.
