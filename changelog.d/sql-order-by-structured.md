### `ORDER BY` over a `jsonb` or range column was an internal error

Both ride as bare Python subdocuments, so the sort's `x < y` raised
`TypeError: '<' not supported between instances of 'dict' and 'dict'` and the
client saw `XX000`.

PostgreSQL's jsonb order was **measured** against 14.13 rather than taken from
the manual, which matters: a top-level empty array sorts before everything,
`null` included. That is not a documented rule but a consequence of storage — a
top-level scalar is held as a one-element array, so `[]` is simply the shorter
container. Nested, `[]` is an ordinary array.

The key has to be decided from the **column**, not the value. Keying only the
values that fail to compare gives an order that is not even transitive, because
Python compares `False < 1` quite happily — `false` landed between two numbers.

#### Fixed

- `ORDER BY` over `jsonb`, ascending and descending, with either NULL
  placement: the full type order, arrays by length then element-wise, objects
  by pair count then key/value pairs walked in PostgreSQL's storage order.
- `ORDER BY` over a range type: empty first, then by lower bound (unbounded
  lowest), then by upper (unbounded highest).
- `OVER (ORDER BY …)` and `array_agg(x ORDER BY x)` over a partition of
  structured values.

#### Still divergent

`'null'::jsonb` and SQL NULL are both Python `None` here, so a JSON null sorts
where SQL NULL does rather than inside the jsonb order. And
`array_agg(x ORDER BY <mixed jsonb>)` builds its sort key inside the pipeline,
where the column type is not available — it no longer errors, but a column
mixing scalars with containers is ordered inconsistently there.
