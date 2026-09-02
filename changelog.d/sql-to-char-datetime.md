### SQL: a real `to_char` / `to_date` / `to_timestamp` template engine

An eighth differential sweep against PostgreSQL 14.13, over datetime
formatting, scored **55 of 122**. The datetime half of `to_char` was built by
converting the template through sqlglot's Postgres `TIME_MAPPING` and handing
the result to `strftime` — a mapping that knows a handful of tokens and matches
single letters anywhere they appear. Two sweeps now score **122/122** and
**190/192**.

#### Fixed

- **Tokens that rendered as their own spelling** now render: `Q`, `W`, `WW`,
  `CC`, `J`, `MS`, `US`, `SSSS`, `HH`, `RM`, `Y,YYY`, `YYY`, `Y`, `IYY`, `IY`,
  `I`, `IDDD`, `FF1`–`FF6`, `TZH`, `TZM`, `OF`, quoted `"literals"`, and the
  `TM` prefix.
- **Tokens matched inside other tokens.** The `D` in `AD` rendered the weekday,
  so `to_char(ts, 'AD')` answered `'A3'` and `'A.D.'` answered `'A.3.'`.
- **Case-sensitive token matching**, which is how Postgres works and is
  observable: `Ddth` is `D` + `d` + `th` (`'44th'`), not `DD` + `th`
  (`'02nd'`). `day`/`dy`/`am`/`bc` now render lower-case.
- **`FM` prefixes one token**, rather than latching on: `FMHH12:MI` is `'2:07'`.
- **`D` is 1=Sunday..7=Saturday**, not the ISO weekday — it was off by one for
  every day of the week.
- **`th` gives the right ordinal** (`'02nd'`, `'01st'`, `'03rd'`), not always
  `'th'`.
- **`to_date` / `to_timestamp` parse word templates.** `Mon`, `Month`, `Dy`,
  `AM`, `MS`, `IYYY IW`, `J` and `DDD` templates raised
  `22007 invalid input syntax` because the same lossy mapping was used to build
  a `strptime` directive.
- **`to_timestamp` returns a `timestamptz`**, not a naive timestamp — it
  rendered without the `+00` offset Postgres sends.

#### Known limitation

A year-less template defaults to **1 BC** on Postgres. Python's `datetime` has
no era and a minimum year of 1 AD, so SecantusDB answers 1 AD. Recorded in
`tasks/backlog.md`; it is the only divergence left in the 314-case sweep.
