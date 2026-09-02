### `isfinite`, `scale`, `cbrt` on a numeric, and `justify_*` on a time

Looking for the opposite of the last round — shapes PostgreSQL accepts and
SecantusDB refused — turned up 21, across five functions.

`isfinite()` and `scale()` were missing entirely. `cbrt()` worked on a whole
number but not on a decimal one, and `justify_hours()` / `justify_days()`
refused a `time`, which PostgreSQL reads as an interval of that length.

`cbrt()` was also inaccurate: `cbrt(1000000)` returned `99.99999999999997`
rather than `100`.

#### Added

- `isfinite()` and `scale()`.

#### Fixed

- `cbrt()` accepts a decimal argument, and is exact for perfect cubes
  (`math.cbrt` on Python 3.11+, a Newton-refined fallback on 3.10).
- `justify_hours()`, `justify_days()` and `justify_interval()` accept a time.

#### Still unsupported

`numnode()` and `strip()` (full-text search), and `hashtext()` — PostgreSQL's
internal hash, whose values cannot be reproduced, so returning a different
number would be worse than refusing.
