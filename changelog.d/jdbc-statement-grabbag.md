### Dollar quotes, nested comments, CTAS, stable now(), and the JDBC escape functions

A grab-bag of SQL-surface fixes driven by pgjdbc's StatementTest and
PreparedStatementTest. Dollar-quoted string literals now work in any
expression position — including digit-bearing tags (`$A0$`) and
tag-vs-content ambiguity (`$B$;$b$B$`) — and nested block comments
(`/* /* */ */`, which PostgreSQL nests) parse correctly. With
`standard_conforming_strings = off`, plain string literals honour
backslash escapes exactly like `E''` strings.

`now()` and `CURRENT_TIMESTAMP` are now transaction-stable: every call
in a statement (and across an explicit transaction block) returns the
same instant, as in PostgreSQL — so interval round-trips like
`extract(second from ((interval '3s' + now()) - now()))` are exact.
`CREATE [TEMP] TABLE … AS SELECT` ships with PG's `SELECT <n>` command
tag, and `TRUNCATE` resolves schema-qualified and session-temp table
names. The scalar-function surface behind JDBC's `{fn …}` escapes is
complete: the trig family (`acos` through `atan2`, hyperbolics, degree
variants), `replace`, numeric-aware `power` and `trunc(x, n)`, and
`to_char`'s word tokens (`Day` / `Dy` / `Month` / `Mon`).

#### Added
- Dollar-quoted string literals (`$$…$$`, `$tag$…$tag$`) in expressions.
- `CREATE [TEMP] TABLE … AS SELECT` (CTAS) with `IF NOT EXISTS`.
- Trig/hyperbolic scalar functions, `atan2`, `cot`, degree variants,
  `replace(text, from, to)`.
- `standard_conforming_strings = off` backslash-escape semantics.

#### Fixed
- Nested block comments mis-tokenized into stray operators.
- `now()` / `CURRENT_TIMESTAMP` drifted between calls in one statement.
- `power` / `trunc` raised TypeError on numeric-vs-double operand mixes.
- `TRUNCATE` dropped the schema qualifier, missing temp and
  schema-qualified tables.
- `to_char` rendered `'Day'` as `'5ay'` (the `D` token matched first).
