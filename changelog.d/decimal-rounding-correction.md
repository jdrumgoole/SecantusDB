### Correction: mongod's decimal transcendentals are largely reproducible after all

An earlier entry concluded from one measurement that mongod is "not correctly
rounded, so no independent implementation can match it". That was **too strong**,
and probing further found the actual rule:

- **`$sqrt` is plain correctly-rounded decimal128** — IEEE 754 requires it for
  square root — and matches on 6 of 6 inputs. Exactly reproducible today, with no
  dependency.
- **`$exp` / `$ln` go through binary128**: rounding the true value to a 113-bit
  significand and converting back to 34 decimal digits reproduces mongod where
  correct *decimal* rounding does not.
- It is **not one uniform rule** — across 24 covered pairs the binary128 route
  matched 20 and missed 4, and the trig/hyperbolic family was not covered.

So there is a real pure-Rust path that needs no C dependency, though it needs a
high-precision core and per-operator verification. The backlog entry now records
the rule, the counter-examples, and the exact next step, instead of a blocker
that was not one.
