### Three operator-typing gaps on the PostgreSQL interface, and one sqlglot mis-parse behind them

Follow-on to the result-type work: the three items that fix left recorded rather
than closed, each measured against a live PostgreSQL 14 before being worked. Two
turned out to have a different root cause than the note predicted, and one
uncovered a second bug sitting next to it.

#### Fixed

- **`pg_typeof(...)` wired as `text` (25) instead of `regtype` (2206).**
  Universal, not specific to any argument: the *value* was right every time and
  only the declared type was wrong, so nothing that compared values could see it.
  `regtype` is now a real type tag. The literal `rewrite_pg_typeof` mints is
  **tagged** rather than wrapped in a `::regtype` cast — the cast is evaluated,
  and `'int4'::regtype` evaluates to the type's OID *number*, so wrapping changed
  the answer from `integer` to `23`. An explicit `'int4'::regtype` now wires as
  regtype too; the `::text` and `::oid` forms are unchanged.
- **A *typed* text operand in arithmetic answered the wrong code.** Postgres
  defines no arithmetic operator on text at all, so the content is irrelevant:
  `'1'::text + 1` errors exactly as `'a'::text + 1` does. We coerced both, so the
  first silently answered `2` and the second reported the coercion's `22P02`
  instead of `42883`. An explicit **cast** is decided in `sql/scalar.py` (a cast
  is unambiguously typed, and a constant-only statement never reaches the
  plan-time analysis); a text **column** is decided in `sql/typecheck.py`, which
  owns the exemptions a declared type needs — reflected schema-on-read tables
  above all, where a column typed `text` from a 50-document sample may hold
  numbers. Postgres names the declared type, so a `varchar` column reports
  "character varying".
- **`interval '1 day' + 1` answered `1 day 00:00:01`.** Not an evaluator bug:
  **sqlglot** absorbs a following NUMBER into its multi-part interval form
  (`INTERVAL '1' DAY '2' HOUR`), a syntax Postgres does not have, so the `1` was
  read as one *second*. The distinction cannot be recovered after parsing —
  sqlglot rewrites the numeric token into a *string* literal inside the
  synthesised `Interval`, leaving `+ 1` and `+ '1'` byte-identical in the AST —
  so it is corrected at the parser, where the token is still visible.
  `+ 'string'` is deliberately untouched: Postgres resolves the unknown literal
  to an interval there, which is what the continuation already computed.
- **`interval '1 day' - 1` answered `22023 cannot delete from scalar`** — a
  *jsonb* error for an interval. Intervals and ranges ride as tagged
  subdocuments, so the `jsonb - key` branch claimed them. Found while probing the
  item above.

`tests/test_sql_operator_types.py` pins the constant forms over the real wire,
asserting the declared OID and the SQLSTATE alongside the value, and compares 22
shapes against a live PostgreSQL when one is reachable.
`tests/test_sql_typecheck.py` grows the column-typed half, including the
reflected-table exemption.
