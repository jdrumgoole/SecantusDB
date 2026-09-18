### A failing gauge run could print a perfect pass rate

`docs/validation-report-php-lib.md` published this, live:

```
| **Overall** | **3089** | **1** | **40** | **3130** | **100.0%** |
```

3089 passed, **one failed**, scored 100.0%. Every report formatted its rate with
`f"{...:.1f}%"`, which rounds — and 3089/3090 is 99.9676%. The website's driver
panels are generated from those reports, so the claim was on the public site.

The threshold is one failure in ~2000 tests, which is exactly where the healthy
gauges now sit: the better the servers get, the likelier their own reports are
to overstate them.

#### Fixed

- One shared `validation_summary.rates.pass_rate` that **floors** to the
  displayed precision, so only a genuinely clean run can print `100.0%`. Raising
  the precision was considered and rejected — `99.98%` still reads as "perfect"
  to someone skimming, whereas `99.9%` beside a visible failure count does not.
- Adopted across 16 per-gauge report generators, the cross-driver summary
  (`validation_summary/generate.py`, including its adjusted rate) and the
  website panels (`driver_panels.py`), replacing nineteen copies of the same
  rounding idiom.
- `tests/test_validation_pass_rate.py` pins both halves: the arithmetic, and
  that a real generator actually *uses* it — verified by reverting one
  generator and watching the test fail.
