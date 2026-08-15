### 65535-parameter statements work; the 1 MB statement cap was too small

The parser guarded against oversized statements with a 1 MB length cap
(a parse-cost DoS guardrail). The premise — "1 MB is far above any real
query" — turned out to be false: a statement using the extended
protocol's full 65535 parameters (`values ($1::text), … ($65535::text)`,
the shape pgx's max-parameter tests exercise) is ~1.04 MB of SQL, and
real PostgreSQL accepts statements up to its 1 GB message limit. The cap
now stands at 16 MB — the same ceiling as the MongoDB document size —
which keeps parse cost bounded while accepting every legitimate shape.

`ParameterDescription` also now wraps its int16 parameter count for
65536-and-up parameters exactly like real PG does (`pq_sendint16`),
instead of crashing the encoder: preparing a 65536-parameter statement
succeeds server-side, with the client responsible for the
65535-parameter execution limit, matching PostgreSQL's behaviour.

#### Fixed

- `sql/planner.py`: `MAX_SQL_LENGTH` raised 1 MB → 16 MB.
- `sql/pgwire.py`: `parameter_description` wraps the int16 count for
  ≥65536 parameters instead of raising `struct.error`.
