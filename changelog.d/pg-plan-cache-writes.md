### The Rust PostgreSQL server reuses the plan of a prepared INSERT or UPDATE

A prepared statement's plan was reused only when its parameters landed in a
`WHERE` clause. An `INSERT` with bound values, an `UPDATE ... SET v = $1` and
an `UPDATE ... SET v = v + 1` were planned again on every execution, which was
a quarter to a third of the server's CPU time for those statements.

#### Changed

- `secantusd-pg` reuses the plan when a parameter is stored into an `int4`,
  `int8`, `float8` or `text` column from a value of the matching kind, and when
  a parameter is read by an expression the server evaluates per row. Server CPU
  per statement, macOS, release build: `INSERT` 49 to 42 us, `UPDATE SET v = $1`
  59 to 47 us, `UPDATE SET v = v + 1` 69 to 57 us, an `UPDATE` matching no row
  45 to 29 us.
- A column with a declared width, a narrower range, a domain, an enum or a
  generated value is planned per execution as before, so its checks see every
  value.

#### Fixed

- `secantusd-pg` holds a `smallint` to its range. `40000::smallint` answered
  40000 and a `smallint` column stored it; both are now PostgreSQL's 22003
  `smallint out of range`. A float outside `integer`'s range (or NaN) cast to
  an integer is 22003 where it used to saturate, and digits too large for the
  type (`'99999999999'::int`) are 22003 `value ... is out of range for type
  integer` where they were 22P02.
