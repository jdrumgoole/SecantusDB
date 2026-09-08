### The Rust PostgreSQL server reports column precision, scale and size

`cursor.description` on the Rust `secantusd-pg` server left `precision`, `scale`,
`display_size` and `internal_size` blank (or, for `internal_size`, a bogus `0`)
for every column. psycopg derives those four attributes from two fields the
server sends in each `RowDescription` — the type modifier (`atttypmod`) and the
fixed byte width (`typlen`) — and the server was sending the pgwire defaults
(`type_modifier = -1`, `type_size = 0`) for all of them. So a `numeric(10,2)`
column reported no precision, a `varchar(42)` no display size, and an `int4` an
internal size of `0` rather than `4`.

The server now emits the real `typlen` for every wire type from a per-type table
(`int4` -> 4, `time` -> 8, `timetz` -> 12, `interval` -> 16, every varlena type
-> -1), and threads the declared modifier of a `select null::type(mod)` cast
through to the wire: `numeric(p,s)`, `varchar(n)` / `char(n)`, `bit(n)` /
`varbit(n)`, and `time` / `timestamp` / `interval` precision are all encoded
exactly as PostgreSQL encodes them (measured field-for-field against PostgreSQL
16). `bit` and `varbit` also now carry their own oids (1560 / 1562) instead of
falling through to `varchar`. This is description metadata only — no value is
decoded any differently.

The whole of psycopg's `test_column.py` now passes against the Rust server (53
of 53, up from 7); the fix clears all 46 previously-failing cases, including the
30 `test_details` and 15 `test_details_time` parametrisations.

#### Fixed

- `secantus-pgserver`: `RowDescription` now sends a per-type `typlen`
  (`type_size`) and a declared `atttypmod` (`type_modifier`); `bit` / `varbit`
  map to their own oids.
- `secantus-pgplan`: a `::type(mod)` cast in a constant SELECT carries its type
  modifier through to the column description.
