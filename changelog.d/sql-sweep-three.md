### `to_char(numeric, …)` matched PostgreSQL on 63 of 300 shapes

A sweep of 30 templates × 10 values against PostgreSQL 14.13 found four rules
the implementation did not have at all, and a fifth problem upstream of it.

* **Overflow prints `#`.** A value too wide for the digit slots fills every one
  of them — `to_char(1234.5, '999')` is `' ###'`, not `' 1235'`. Printing the
  number anyway silently violated the template's own declared width.
* **The sign sits against the digits**, immediately left of the first one, not
  in front of the padded field: `to_char(-12, '999')` is `' -12'`.
* **A `0` slot zero-fills everything to its right**: `'0999'` over 12 is
  `' 0012'`.
* **An all-`9` integer part renders blank when the value has none** —
  `to_char(0.5, '999.9')` is `'    .5'`.

And the template never reached the numeric formatter intact: sqlglot's postgres
dialect part-converts it to strftime first, so `MI999` arrived as `%M999` and
`9999D99` as `9999%u99` — the tokens the numeric formatter did not recognise
were simply dropped, which is why `D` produced no decimal point at all.

All 300 shapes now match.

#### Fixed

- `to_char(numeric, …)`: overflow `#` fill, sign placement (`S` / `MI` / `PR`,
  leading and trailing), `0` zero-fill, blank integer parts, `G` / `D` / `L`
  locale tokens, `$` in front of the sign, `FM` (which drops trailing
  fractional zeros but keeps the point), and `RN` Roman numerals.
- `IN (subquery)` and `NOT IN (subquery)` in `UPDATE` and `DELETE`. The
  identical predicate has always worked in a `SELECT` — the DML planners simply
  never published the subquery context the `SELECT` planner does.

#### Corrected tests

Six `to_char` expectations recorded this engine's own output rather than
PostgreSQL's, and were re-probed against 14.13: `FM` drops trailing fractional
zeros (`FM$9,999.99` over 1234.5 is `$1,234.5`), a value too wide for its slots
is `###.##`, and `L` is the *locale* currency symbol — empty here, not `$`.

#### Still divergent

`EXISTS (subquery)` in `UPDATE` / `DELETE`, `UPDATE … FROM (VALUES …)`, and
`string_agg` / `array_agg` with `DISTINCT`.
