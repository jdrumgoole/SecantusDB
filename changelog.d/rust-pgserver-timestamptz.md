### Timestamps that know their time zone

`timestamptz` is not a timestamp with an offset stapled to it. It is an instant,
and what you see is the session's view of that instant — so the same stored
value prints as `12:00+01` in Rome and `06:00-05` in Chicago, and the same
literal read under two zones names two different moments. The Rust PostgreSQL
server now supports it, along with `timetz`, `SET TimeZone` for both fixed
offsets and named IANA zones, and the `regtype` that `pg_typeof` had already
been answering with.

Two sign conventions meet in this type and they run in opposite directions.
In `SET TimeZone TO '+02:00'` the sign is POSIX: positive means *west* of
Greenwich, so that setting renders timestamps as `-02`. In a literal like
`'2026-01-01 12:00+02'` the sign is the ordinary one, two hours *east*. Both
were measured against a live PostgreSQL rather than reasoned out, because
getting either backwards is completely invisible under UTC and wrong by hours
everywhere else.

Named zones carry their daylight-saving rules, so `2026-01-01 12:00` and
`2026-07-01 12:00` resolve to different offsets in `Europe/Rome` and to the same
one under a fixed `+02:00`. Offsets may also carry minutes and seconds:
`+01:02:03` is a real offset that appears in client test suites, and a comment
in this work asserting otherwise was contradicted by the first probe that looked.

#### Added

- `timestamptz` and `timetz`, as casts, literals and bound parameters in both
  wire formats, with their own type oids.
- `SET TimeZone` for fixed offsets (`'+02:00'`) and named IANA zones
  (`'Europe/Rome'`), including daylight-saving rules.
- `regtype` as a cast target: `'int4'::regtype` is `integer`.
- Offsets with minute and second precision.

#### Fixed

- A doc comment that had come adrift from the function it described.

#### Changed

- A `timestamptz` or `timetz` *column* is now refused rather than accepted. The
  types are stored as canonical text, and a timestamptz renders in the session's
  zone — so a row written under UTC read back under another zone showed the right
  instant with the wrong wall clock and the wrong offset, which no client could
  detect. They remain available everywhere they are a value rather than storage.
