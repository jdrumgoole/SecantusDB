### `sum()` of a float column no longer breaks the client

On the Rust PostgreSQL server, `select sum(f)` over a `float8` column
described the result as `int8` and then sent `1.5`, so psycopg raised
`invalid literal for int() with base 10: '1.5'` — an exception, on an
ordinary query, rather than a number.

`sum()`'s result type was "numeric if the input was numeric, otherwise int8".
PostgreSQL's widening is not uniform, and now neither is ours (measured
against 14.24):

| input | `sum` |
| --- | --- |
| `int2`, `int4` | `int8` |
| `int8` | `numeric` |
| `float4` | `float4` |
| `float8` | `float8` |
| `numeric` | `numeric` |
| `money` | `money` |
| `interval` | `interval` |

So `sum(int8)` was wrong too: it claimed `int8` where PostgreSQL returns
`numeric`.

#### Fixed

- `crates/secantus-pgplan`: `sum_result_type`, shared by the planner's output
  definition and the wire type.
- `crates/secantus-pgserver`: the row description uses it.

#### Testing

- `tests/test_rust_pgserver_slice.py`: every numeric input type's `sum` oid
  and value.
