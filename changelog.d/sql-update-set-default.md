### `UPDATE … SET col = DEFAULT`

Setting a column back to its default reported `column "default" does not
exist`. The `DEFAULT` keyword was being read as the name of a column.

A column with no default becomes NULL, as in PostgreSQL, and a quoted
`"default"` still means a column called `default`.

#### Fixed

- `UPDATE … SET col = DEFAULT` sets the column to its default, for a literal
  default, an expression default, or NULL where there is none. A `NOT NULL`
  column with no default reports a not-null violation, as PostgreSQL does.
- The same mis-reading sat under the guard that decides whether a generated
  column may be updated, so that check was treating every `SET gen = DEFAULT`
  as a non-DEFAULT value.

#### Known limitation

`SET serial_col = DEFAULT` is refused rather than guessed at: a serial's
default draws from its sequence, which the statement planner cannot do.
