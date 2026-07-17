### SQL server: CREATE TYPE … AS RANGE

User-declared range types land: `CREATE TYPE textrange AS RANGE (subtype =
text)` mints the type and its auto-created companion multirange
(`textmultirange`, following Postgres' naming rule) with allocation-stable
OIDs, reflects both through `pg_type` (typtype `r` / `m`, real `typarray`)
and `pg_range` (`rngtypid` / `rngsubtype` / `rngmultitypid`), and wires the
full value path: literal casts (`'[a,b)'::textrange`,
`'{[a,b)}'::textmultirange`) parse with the declared subtype's coercion, the
type gets its constructor (`textrange(lo, hi, bounds)`), parameters a
registered psycopg dumper declares with the minted OID round-trip in text and
binary (PG's range wire layout), results describe with the minted OID and
render/encode as ranges in both formats, and `DROP TYPE` removes the pair.
psycopg's `RangeInfo.fetch` → `register_range` → typed `Range` values works
end-to-end, as does the multirange counterpart.

The statement itself exceeds sqlglot's parser (it falls back to a raw
Command), so the engine intercepts the command tail — the same pattern
`CREATE DOMAIN` uses. Together with the earlier waves this clears psycopg's
custom-range fixtures, which previously errored out of thirty-one range and
multirange tests before any assertion ran.

#### Added

- `catalog.py`: `create_range_type` / `get_range_type` (by range or companion
  multirange name) / `drop_range_type` / `list_range_types`, minted from the
  stable user-type OID counter; `multirange_name_for` (Postgres' rename rule).
- `engine.py`: the `CREATE TYPE … AS RANGE (…)` Command interception (subtype
  resolved via regtype spelling; collation/opclass options accepted, ignored);
  `DROP TYPE` drops range types.
- `ranges.py`: `custom_elem` parsing/construction for non-builtin subtypes.
- `scalar.py`: custom range/multirange casts and constructors.
- `pgextended.py`: binary custom-range/multirange parameter decode and the
  generic binary range/multirange result encoders; user-type binary params
  route by catalog kind.
- `virtual.py`: `pg_type` + `pg_range` rows for user ranges; `regtype` /
  `user_type_*` resolution covers them.
