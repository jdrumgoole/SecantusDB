### Five ways a SQL connection could drop with "internal error"

Every crash the PostgreSQL front end reported as a bare `internal error` came
from a distinct, small cause, and each one killed the connection rather than
returning a message the client could act on. All five are fixed, along with a
quadratic cost in parameter binding that made large statements look like hangs.

The protocol's 16-bit count fields — parameter counts, column counts — were
read and written as *signed*. Postgres allows up to 65535 parameters in a
single Bind, and a JDBC driver rewriting a batch into one statement really does
send tens of thousands; above 32767 the count came back negative, walked the
parse offset backwards, and the connection died. Binding those parameters was
also `O(N²)`, because each placeholder was replaced one at a time and the
expression library re-parents every sibling on each replacement. A statement
with 40000 parameters took over two minutes; it now takes well under a second.

Geometric values had no binary decoder at all, so a `point`, `box` or `polygon`
sent in the binary format — which drivers do by default — arrived at the *text*
parser as raw bytes and failed as "no coordinate pairs in geometry". The `line`
type could not be parsed even as text: its canonical form is three coefficients
`{A,B,C}` rather than coordinate pairs, and the branch that handled it sat
after the pair parse it could never survive. `time + interval` was simply
missing, and an interval inside a `WHERE` clause was pushed down into an
aggregation expression that has no interval type, where it surfaced as a
`$multiply` type error. Finally, the catalog builders behind `pg_class` and
friends enumerated the table list twice — once to assign OIDs and once to emit
rows — so a table created by another session in between produced a `KeyError`
part-way through a catalog scan.

#### Added

- Binary parameter decoders for every geometric type: `point`, `lseg`, `path`,
  `box`, `polygon`, `line`, `circle`.
- `pggeo.line_from_points`, converting the two-point spelling of a `line` to
  its `{A,B,C}` canonical text the way Postgres does.
- `virtual._tables_with_oids`, the single-snapshot accessor catalog builders
  use instead of enumerating the tables twice.

#### Fixed

- 16-bit count fields are read and written unsigned, so a Bind carrying more
  than 32767 parameters no longer drops the connection. Fields that can
  legitimately be negative — attnum, type size, format codes — stay signed.
- Binding N parameters is linear rather than quadratic.
- `line` values parse, and an open `path` keeps its `[…]` spelling through a
  round trip instead of being rewritten as closed.
- `time ± interval` returns a `time`, wrapping into a single day and dropping
  the month/day components, as Postgres does. `timetz ± interval` does the same
  and carries the zone offset through untouched.
- A `date` compared against a computed `timestamp` promotes to midnight the way
  Postgres does, instead of failing to compare ISO text against a datetime.
- `'23:59:60'::time` carries forward to `24:00:00` rather than storing a second
  that nothing downstream could parse — which had made `time - time` fail too.
- An unknown-type operand beside an interval resolves numerically, so
  `$1 * $2::interval` works with the typeless parameters JDBC drivers bind.
- Interval arithmetic in a `WHERE` clause falls back to per-row evaluation
  instead of lowering to an aggregation expression that cannot express it.
- Catalog builders take one snapshot of the table list, so concurrent DDL no
  longer aborts a `pg_class` / `pg_attribute` / `pg_attrdef` / `pg_description`
  / `pg_index` scan.
