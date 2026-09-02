### `AT TIME ZONE`, `LIKE ALL/ANY`, and the `timezone_*` extract fields

Three features the sixth sweep found refused, taking that sweep from 20 of 33
matching PostgreSQL 14.13 to 30.

#### Added

- **`AT TIME ZONE`** — the SQL operator, not a function. It reads both ways,
  and which way depends on the operand: a *naive* timestamp is interpreted as
  being in the zone and becomes an instant, while an *aware* one is converted
  into the zone and loses it. So `'2020-06-15 12:00'::timestamp AT TIME ZONE
  'America/New_York'` is 16:00 UTC, and the same instant back through it is
  08:00. An unknown zone is `22023`.
- **`LIKE ALL(<array>)` / `LIKE ANY(<array>)`**, and their `ILIKE` and
  `NOT LIKE` spellings. The scalar `LIKE` worked; the quantified form was
  `0A000 unsupported scalar expression`.
- **`extract(timezone …)` / `timezone_hour` / `timezone_minute`**, reporting
  the **session** zone's offset. PostgreSQL normalises a timestamptz into the
  session zone before extracting, so `'…+05'::timestamptz` gives 0 under a UTC
  session rather than 5.

#### Fixed

- The identity and generated-column insert errors put their explanation in
  **DETAIL**, as PostgreSQL does, instead of folding it into the message —
  which had made a message no client could match on. The `HINT` matches too.

#### Still divergent

`to_ascii()` and `IS NORMALIZED` (which parses as a column reference), and
`regexp_count` / `regexp_instr`, which PostgreSQL 14 does not have either —
only the error differs.
