### `FROM unnest(...)` on the Rust PostgreSQL server

`SELECT * FROM unnest(ARRAY[1,2,3])` is how a client turns an array into rows,
and the Rust PostgreSQL server answered `0A000 … is not supported yet`. So did
`regexp_split_to_table` and `generate_subscripts`, and so did every clause a
client puts around one of them — the `ORDER BY`, the `WHERE`, the
`count(*)`.

All of that works now. A set-returning function in `FROM` supports table and
column aliases, `*` expansion, filtering, ordering, limiting and aggregates; an
empty or NULL array yields no rows rather than an error; and a multidimensional
array unnests to its *leaves* in row-major order, so `unnest(ARRAY[[1,2],[3,4]])`
is four rows rather than two. The same functions work as a bare target with no
`FROM` at all.

The interesting part is how little code it took. The executor already had the
right shape for this — its own comment explains that a generated source "is a
SOURCE rather than its own statement" precisely because everything downstream
works on documents and does not care where they came from. So a set-returning
function is now planned as the same thing `FROM (SELECT …) s` produces, and the
FROM-subquery path supplies all the clause handling for free. Nothing in the
executor changed, and the two spellings of `unnest(ARRAY[1,2])` — as a target
and as a `FROM` item — share one implementation, so they cannot drift apart.

`generate_series` keeps the lazy path it already had. It is a *range* rather
than a bounded list, and materialising `generate_series(1, 10000000)` into a
vector to gain uniformity would have been a clear regression.

#### Added

- `unnest`, `generate_subscripts` and `regexp_split_to_table` as a `FROM` item,
  with `WHERE`, `ORDER BY`, `LIMIT`, aggregates (`count(*)`, `array_agg`), `AS
  t(col)` and `AS t` aliasing, and `*` expansion.
- The same three as a bare select-list target with no `FROM`.
- `unnest` over a multidimensional array, yielding the leaves in row-major
  order.

#### Known limitations

- A set-returning function in the select list **over a column** —
  `SELECT unnest(ia) FROM t` — is still refused by name. That form changes the
  row count mid-pipeline rather than supplying the source, which is a different
  mechanism from the one added here.
- `unnest(a, b)` with several arrays zips and NULL-pads them; it is refused by
  name rather than answered as a single column.
- `LATERAL` forms are unaffected: a function in `FROM` that references the row
  to its left needs correlation machinery, not a materialised source.
