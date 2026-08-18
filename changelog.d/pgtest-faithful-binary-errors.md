### Faithful errors for out-of-scope binary parameters

An untyped (`unknown`, oid 0) binary bind parameter whose bytes aren't valid
text — for example an EWKB `GEOMETRY` value bound to `$1::GEOMETRY`, a type
SecantusDB's core-PostgreSQL SQL layer doesn't model — no longer leaks a
`UnicodeDecodeError` as a generic `XX000` internal error. It now surfaces
PostgreSQL's `22P03` (invalid binary representation), so the connection
recovers cleanly with an honest error. PostGIS (`GEOMETRY` / `BOX2D`) and
pgvector (`VECTOR`) remain out of scope for the surrogate; the pgtest gauge
records them as documented divergences rather than unexpected failures.

#### Fixed

- `pgextended.py`: a non-text binary payload for an untyped parameter raises a
  faithful `22P03` instead of leaking a `UnicodeDecodeError` (→ `XX000`).

#### Changed

- pgtest gauge: `spatial`, `box2d`, and `pgvector` are documented expected
  divergences (extension types outside the surrogate's core-PostgreSQL scope).
