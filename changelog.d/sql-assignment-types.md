### `UPDATE … SET col = value` rejects a value PostgreSQL would not assign

`UPDATE t SET int_col = text_col` and `UPDATE t SET bool_col = 1` were silently
coerced. PostgreSQL rejects both with `42804 column "…" is of type … but
expression is of type …`, because assignment goes through *assignment casts* —
a different, more permissive rule than the implicit casts a comparison gets,
which is why this needed its own analysis rather than reusing the existing
cross-category comparison check.

The rule is the one PostgreSQL actually applies, measured across 25 shapes: a
string target accepts anything (there is an assignment cast to `text` /
`varchar` / `char` from every type involved), an expression whose type is not
statically certain is left alone so a bad value still fails at runtime where
PostgreSQL fails it, and otherwise a type-category mismatch is the error. That
keeps `bigint_col = int_col`, `int_col = real_col` and `int_col = 1.7` working
while rejecting `int_col = text_col`.

Two coercion errors turned up in the same probe run and are fixed with it.

#### Fixed

- `UPDATE t SET numeric_col = 'abc'` reached the client as a raw
  `[<class 'decimal.ConversionSyntax'>]` — a Python exception on the wire. It
  now reports `22P02 invalid input syntax for type numeric: "abc"`.
- Date, time and timestamp values that cannot be parsed now report
  PostgreSQL's `22007 invalid input syntax for type date: "nope"` rather than
  an internal message, and `timestamp` coercion uses `22007` rather than
  `22P02`.

#### Changed

- The `SERIALIZABLE` isolation-level divergence is now named in the SQL
  capability matrix, not only in the transactions section, and the
  documentation records why accepting the level and documenting it was chosen
  over rejecting it or reporting `repeatable read`.

#### Infrastructure

- CI now runs the PostgreSQL-oracle suites against a real PostgreSQL. They
  compare SecantusDB's answers against a live server, and until now only ever
  executed on a developer machine that happened to have one — in CI they
  skipped silently. A Linux-only job stands up a pinned `postgres:14.13` and
  fails if the server is unreachable, so the suites cannot go back to skipping
  unnoticed.
