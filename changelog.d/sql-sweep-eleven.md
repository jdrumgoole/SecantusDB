### An eleventh SQL sweep: a multidimensional array is one array, not nested ones

Sequences and identity columns came back strong (23 of 26 shapes already
matching PostgreSQL 14.13). Arrays did not — 20 of 29, with the misses clustered
on **multidimensional** arrays, which PostgreSQL does not treat as nested arrays
at all: `int[][]` is ONE array with two dimensions, and every whole-array
operation walks it flat.

**`array_to_string` leaked Python syntax.** Joining only the top level rendered
each inner list through `str()`, so `array_to_string(ARRAY[[1,2],[3,4]], ',')`
answered `[1, 2],[3, 4]` where PostgreSQL says `1,2,3,4`.

**`unnest` of a 2-D array crashed** with a bare `invalid literal for int():
'{1,2}'` and *no SQLSTATE* — the inner lists went out as elements and the int4
output coercion died on them.

**Every scalar subquery over a `VALUES` source failed** with `42P01 relation ""
does not exist`. Not an array bug at all — `IN (SELECT … FROM (VALUES …))`,
`EXISTS`, `ARRAY(…)` and the bare scalar form were all affected, because a
VALUES-derived source is not a relation and the inner-table lookup found
nothing.

#### Fixed

- **`array_to_string` and `unnest` walk a multidimensional array flat**, in
  row-major order. `unnest` had **three separate copies** of the same one-level
  logic — the `exp.Unnest` node, the Anonymous spelling, and the select-list
  expansion — which is why fixing one of them was not enough; they share a
  helper now.
- **A `VALUES` source works inside any scalar subquery**, with the row's cells
  evaluated on reference so an expression in the VALUES list works too, and the
  alias's column names (or PostgreSQL's default `column1`, `column2` …) both
  resolve.
- **A scalar subquery and `ARRAY(subquery)` keep their element type.**
  Everything came back as `text` before: `(SELECT n FROM (VALUES(7)) t(n))` is
  `int4`, and `ARRAY(SELECT n …)` is `int4[]`.
- **`pg_get_serial_sequence`** returns the schema-qualified sequence name
  instead of NULL. The column already recorded it (`Column.sequence`, which
  `nextval` and the `information_schema` view both read) — this just never
  looked, so ORM reflection saw every serial column as plain.

#### Added

- **`ARRAY(SELECT ...)`**, the array-subquery constructor, which parses as an
  `Array` whose single element is the `Select` and so tried to evaluate a
  `Select` as a scalar.
