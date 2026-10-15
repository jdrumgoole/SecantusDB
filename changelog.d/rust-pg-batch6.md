### Rust PostgreSQL server: backlog batch 6

PostgreSQL 15's built-in functions that a catalog role normally reaches --
the types' output and send functions, the B-tree and hash support
functions, the aggregates' transition and final functions -- now answer
when a statement calls them by name. `tools/probes/pg_builtin_coverage.py`
went from 1,634 to 1,890 of 1,934 functions answering as PostgreSQL 15.19
does.

#### Added

- The types' output functions (`int4out`, `date_out`, ... every
  `typoutput`), each its argument's I/O conversion to a new `cstring` type
  (oid 2275: a `char(n)` keeps its padding, a `bool` is `t` / `f`); the
  `*typmodout` functions; and every `typsend`, answering the binary wire
  format through the encoder the wire layer uses (`xid`, `xid8`,
  `regconfig`, `tsvector`, `tsquery` and `jsonpath` in formats of their
  own).
- The hash support functions, transcribed from PostgreSQL's `hashfn.c`
  (Jenkins' lookup3) and each type's hash function: `hashint4`,
  `hashtext`, `hash_numeric`, `hashinet`, `hashtid`, `timetz_hash`,
  `hash_array`, `hash_range`, `hash_multirange`, `jsonb_hash`, and every
  `*extended` form with its seed.
- The B-tree comparison functions that answer -1 / 0 / 1, `in_range` for
  every window-frame type, and the `*larger` / `*smaller` functions.
- The aggregates' transition, combine and final functions over their
  state arrays (`float8_accum`, `float8_regr_accum`, `int4_avg_accum`,
  `interval_accum`, `int8inc`, `booland_statefunc`, ...), with
  PostgreSQL's errors for a state of the wrong length.
- Fourteen functions behind operators (`box_overlap`, `dist_lp`,
  `pg_lsn_pli`, `ts_match_vq`, ...), and the operators `line ?# box`,
  `lseg ?# line` and `line <-> line`.

#### Fixed

- `jsonb - text` with a typed text key was 42883.
- An `aclitem` naming a role oid that no role has could not be put in an
  array.
- `pg_catalog.timestamp(date, time)` and `timestamptz(date, time[tz])` were
  taken for a modifier's coercion and refused.
- An unrecognised interval modifier is XX000, as on PostgreSQL.
- `close_ls` of a segment parallel to the line is NULL.
- An infinite timestamp compared with a finite one was 0A000.

Pinned by `tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed`
(RP-015); what RP-015 still lists -- the comparisons that answer a
`memcmp` difference -- needs a decision.
