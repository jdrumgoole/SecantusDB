### The Rust PostgreSQL server supports avg()

`avg()` was refused with a note that it "returns PostgreSQL numeric with its
own scale rules; approximating it would be a wrong answer".

Those rules turn out to be the ones numeric DIVISION already follows here: a
small quotient gets 16 decimal places, and an input carrying more keeps more —
`avg` of two values with 20 decimals answers 20. So `avg` is the exact sum
over the count, divided through the same code the `/` operator uses, and the
scale is right without anyone deciding it twice.

A float input averages as `float8`; the integers and `numeric` answer
`numeric`; an empty input is NULL. It composes with `DISTINCT`, `FILTER` and
`HAVING`.

#### Added

- `crates/secantus-pgplan`: `AggFunc::Avg` and `avg_result_type`.
- `crates/secantus-pgserver`: the computation, over the exact sum.

#### Testing

- `tests/test_rust_pgserver_slice.py`: integer, float and numeric inputs, a
  20-decimal input, the reported oids, an empty input, and avg beside
  DISTINCT, FILTER and HAVING — measured against PostgreSQL 14.24.
