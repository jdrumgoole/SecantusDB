### Describe stops executing volatile functions, regclass resolves, and more

The extended protocol's Describe no longer *executes* volatile functions
to learn a statement's shape: `Describe select pg_sleep(5)` actually
slept (and a CancelRequest arriving mid-sleep was swallowed into a
NoData reply while Execute later emitted a DataRow — the protocol
violation JDBC's `setQueryTimeout` crashed on), and a Describe of
`nextval(...)` drew a sequence value. Known volatile session functions
now describe from a static type table, cancellation interrupts Execute
where it belongs, and the sequence is untouched until execution.

`'name'::regclass` resolves to the relation's pg_class oid — including
schema-qualified spellings and search_path resolution for bare names —
while still rendering as the relation name, so metadata queries joining
`c.oid = ?::regclass` work (pgjdbc's SearchPathLookupTest). Slash-format
timestamp input (`'8/10/7777'`) parses per the session's DateStyle field
order (MDY/DMY), `array_fill(value, ARRAY[dims])` lands, and geometric
results (point, lseg, box, path, polygon, line, circle) have binary-mode
encoders, completing pgjdbc's binary PGpoint/PGbox round-trips.

#### Added
- `regclass` casts resolve names to pg_class oids (42P01 when unknown);
  `regtype` casts resolve base types and table row types to pg_type oids,
  with search_path resolution and bare-typname rowtype rows in pg_type.
- Slash-format DateStyle-aware timestamp input.
- `array_fill` (value + dimensions form).
- Binary result encoders for the seven geometric types.

#### Fixed
- Describe evaluates no volatile function (pg_sleep, nextval, setval,
  set_config, lo_*, advisory locks); cancels land in Execute with 57014.
