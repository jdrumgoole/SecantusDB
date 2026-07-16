### SQL server: the psycopg JSON and datetime suites go fully green

Two of the three biggest failure clusters in the psycopg conformance gauge —
`tests/types/test_json.py` (181 failing) and `tests/types/test_datetime.py`
(259 failing) — now pass completely, taking the gauge headline from 2900
passed (70.3%) to 3473 passed (84.2%) under deterministic test order. The
third cluster, `test_typing.py` (125), was purely environmental: it shells out
to a bare `mypy`, which the gauge venv didn't carry — mypy now rides the `dev`
extra and all 125 pass.

The JSON cluster came down to one root cause with wide blast radius: json and
jsonb values were never parsed at ingress. A `'{"a":1}'::jsonb` cast passed
raw text through, so `->`/`->>` navigation returned NULL and output
double-encoded. Casts and json-declared parameters now parse into real JSON
values, `array[…]::text` renders Postgres' `array_out` literal instead of a
JSON list, `E'…'` escape strings evaluate (psycopg's `sql.Literal` emits them
for any string containing a backslash), and the plain-json OIDs (114/199)
alias the jsonb tag.

The datetime cluster decomposed into seven root causes, all fixed: temporal
parameters substituting as bare text (a datetime param silently compared
false against an equal cast literal); interval literals rejecting PG's unit
abbreviations (`1s`, `5 min`, `1d 3h`); parser gaps for `epoch`, `infinity`,
BC dates, non-padded fields and loose UTC offsets; the session `TimeZone` GUC
being ignored on both input and output (including POSIX-inverted numeric
zones and `set_config()`, which now emits ParameterStatus); `DateStyle`-aware
text rendering (German/SQL/Postgres orders); binary encoders using float
seconds (a 1µs error at year 9999) and lacking infinity sentinels; and
PG-range values beyond Python's datetime limits, now carried as text via
proleptic-Gregorian ordinal math so `'9999-12-31'::date + 1` returns
`10000-01-01` like a real server. Intervals also gained PG's justified
duration comparison (`-1 day +23:59:59.999999 = -0.000001s`).

#### Added

- `datetimes.py`: proleptic-Gregorian ordinal helpers valid outside
  [year 1, 9999], `infinity`/`-infinity`/`epoch` sentinels, wide/BC timestamp
  canonical text + binary wire values, `TimeZone`-GUC tzinfo resolution
  (POSIX sign convention, zoneinfo names), loose-input widening.
- `intervals.py`: PG unit abbreviations (`s`/`sec`/`min`/`h`/`d`/`w`/`y`/
  `ms`/`us`, attached forms like `1d`), justified `total_micros`.
- `typemap.py`: session-bound render context (TimeZone/DateStyle GUCs honoured
  at output), typed parameter carriers (`JsonText`/`DateText`/`TimeText`/
  `TimeTzText`) that substitute as casts, `json` OID aliases.
- `session.py`: case-insensitive GUC name canonicalization (`set timezone`
  hits `TimeZone`); `set_config()` on a reportable GUC emits ParameterStatus.
- `pyproject.toml`: `mypy` in the `dev` extra (psycopg's `test_typing.py`
  shells out to it).

#### Fixed

- `planner._value_to_node`: datetime / date / time / timetz / interval / json
  parameters substitute as typed casts, not bare string literals — the same
  treatment `Decimal` already had.
- `pgextended.py`: temporal text params convert per their declared OID; binary
  interval params decode to the interval subdoc; binary timestamp/date
  encoders use integer-µs arithmetic and PG's infinity sentinels.
- `scalar.py`: `'nope'::timestamp` raises `22007` instead of silently passing
  raw text into the binary encoder; `ts::text` renders through the
  session-aware renderer; mixed naive/aware datetime comparisons treat naive
  as UTC; multi-value `SET name = v1, v2` (DateStyle) is stored and reported.
- `engine.py` / `functions.py`: `client_encoding` canonicalises on every SET
  path (`utf-8` → `UTF8` in ParameterStatus).
