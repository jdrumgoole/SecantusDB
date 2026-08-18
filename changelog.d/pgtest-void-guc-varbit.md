### Wire fidelity for void returns, custom GUCs, binary bit params, and unknown params

Four PostgreSQL wire-protocol fidelity fixes surfaced by CockroachDB's
pgtest corpus. `pg_sleep` now reports its result as the `void` type (OID
2278, typlen 4) with a NULL value, matching real PostgreSQL, instead of an
untyped text column. A custom (extension) GUC spelled `namespace.name`
survives a round trip: `SET custom_option.session_setting = 'abc'` followed
by `SHOW custom_option.session_setting` returns `abc` — previously the SET
dropped the namespace prefix, so SHOW never found the value. A binary
`bit` / `varbit` bind parameter is now decoded through PostgreSQL's
`varbit_recv` framing: an empty payload (too short for the 4-byte bit-length
header) raises `08P01`, and a payload whose declared bit length leaves
trailing bytes unconsumed raises `22P03`, where before the raw bytes were
silently accepted as text. And a parameter a client declares as the
`unknown` type (OID 705) is now resolved from context — the INSERT target
column, a cast, a compared operand — exactly like an undeclared parameter,
so `ParameterDescription` reports the resolved types instead of echoing 705.

#### Fixed

- `functions.py` / `typemap.py` / `pgwire.py`: `pg_sleep` returns the
  `void` type (OID 2278, size 4) with a NULL value (pgtest `void`).
- `engine.py`: `SET namespace.name = …` preserves the full dotted custom-GUC
  name so `SHOW namespace.name` resolves it (pgtest `set`).
- `pgextended.py`: binary `bit` / `varbit` parameters decode via `varbit_recv`
  framing — short payloads are `08P01`, unconsumed trailing bytes are `22P03`
  (pgtest `varbit`).
- `planner.py`: an explicitly-declared `unknown` (OID 705) parameter is
  treated like an undeclared one, so parse-analysis type inference resolves it
  (pgtest `unknown`).
