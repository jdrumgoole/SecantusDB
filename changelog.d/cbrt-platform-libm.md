### `cbrt` is judged against the platform's libm, not a hardcoded 3.0

Three `cbrt` tests asserted that `cbrt(27.0)` is exactly `3.0`. That is what
Windows and macOS answer — and it is not what Linux answers. PostgreSQL's
`dcbrt` is a bare libm `cbrt()` call, libm is not correctly rounded, and glibc
2.39 returns `3.0000000000000004` for that input: exactly one ULP high, while
8, 64, 125 and 1e6 come out exact. So PostgreSQL on Linux returns
`3.0000000000000004` too, and the assertion was pinning the wrong platform's
answer.

The tests now assert to within one ULP, plus a separate check that we hand
libm's value through byte-for-byte — which is the property that matters, since
being *more* exact than libm would move us away from PostgreSQL. `_real_cbrt`
itself is unchanged; its docstring already named this trade-off correctly.

Worth recording how it hid for so long: a push or PR run tests only Python 3.10
on Linux, and 3.10 has no `math.cbrt`, so it takes a Newton-refined fallback
that rounds the ULP away and passes. Only the full 3.10–3.13 matrix — a
`workflow_dispatch` or the weekly cron — runs the Python versions that fail. It
had been red in the cron before anyone looked.

#### Fixed

- `tests/test_sql_missing_builtins.py`: `cbrt` expectations are no longer
  hardcoded floats. Verified by emulating glibc's `cbrt(27.0)` on a non-Linux
  box: the old assertions fail with exactly the three names CI reported, the new
  ones pass.

#### Added

- A test that a 1-ULP libm value survives the SQL layer unrounded, so nothing
  between the function and the result row quietly "corrects" it.
- Two backlog entries: the ≤1 ULP divergence the 3.10 fallback causes against
  PostgreSQL on Linux (a record, not a task — it disappears when 3.10 support
  does), and the CI matrix gap that let a 3.11+ Linux failure stay invisible to
  every pull request.
