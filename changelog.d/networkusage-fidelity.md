### Reply-shape fidelity: repeated ?column?, array-cast names, int4 series

Three small divergences that pgx's byte-exact network-usage test caught
in one reply:

Real PostgreSQL repeats duplicate output column names verbatim —
`select 'a', 'b'` describes as `?column?, ?column?` — where we suffixed
them (`?column?_2`). The evaluated-select path now keeps PG's names
(its row extraction is positional, so uniquifying was never
load-bearing there). An unaliased array cast is named after its ELEMENT
typname, like PG (`'{a}'::text[]` yields a column named `text`). And
`generate_series` with int4-range bounds now yields int4 rows (oid 23,
4-byte binary cells) instead of int8, matching PG's overload selection
— with describe-time (unbound parameter) and execute-time typing
agreeing, so a RowDescription never claims int8 over int4 cells.

#### Fixed

- `sql/planner.py`: the evaluated-select path stops uniquifying display
  names; `_cast_output_name` names array casts after the element type.
- `sql/srf.py`: `generate_series` types int4 for int32-range bounds
  (int8 otherwise), consistently between Describe and Execute.
