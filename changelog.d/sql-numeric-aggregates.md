### `avg`, `stddev` and `variance` over exact types answered the wrong type

PostgreSQL accumulates N, sum(X) and sum(X²) as numerics and finishes in
numeric arithmetic, so an integer or numeric input gets an exact `numeric`
answer whose scale comes from `select_div_scale`. This engine used Mongo's
float accumulators — `$avg`, `$stdDevSamp` — and squared the stddev for the
variances. Every one of them came back `float8` where PostgreSQL says
`numeric`, with the last digits wrong and no scale at all:
`2.333333333333333` for PostgreSQL's `2.3333333333333333`.

#### Fixed

- `avg`, `stddev`, `stddev_samp`, `stddev_pop`, `variance`, `var_samp` and
  `var_pop` over `smallint` / `integer` / `bigint` / `numeric` are computed
  exactly and reported as `numeric`, at PostgreSQL's derived scale — `avg(i)`
  over 1, 2, 4 is `2.3333333333333333`, and over a single 1 it is
  `1.00000000000000000000`.
- A non-positive numerator short-circuits to a plain `0`, as PostgreSQL's
  `const_zero` does. Dividing instead would have given
  `0.00000000000000000000` for a constant column.
- `variance` and `var_pop` inside a computed projection (`variance(i)::text`)
  work at all; they were `0A000 aggregate variance is not supported`.
- A float input now reports `float8`, which is what PostgreSQL answers. The
  variances claimed `numeric` for a value that had been a float all along —
  an existing test asserted that, and has been corrected against the reference
  server.

#### Still divergent

`stddev` / `variance` over a **float** input differ in the last bit or two:
PostgreSQL uses the Youngs-Cramer update there, which is a different rounding
path from summing the squares. `avg(DISTINCT …)` and an aggregate with a
`FILTER` clause keep the float path.
