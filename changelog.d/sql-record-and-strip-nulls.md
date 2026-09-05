### A record rendered as JSON, and half of strip_nulls

#### Fixed

- `record::text` renders PostgreSQL's record literal — `('a', 1)::text` is
  `(a,1)`, not `{"f1": "a", "f2": 1}`. The record renderer already existed and
  is what the wire uses for a composite column; only the cast did not route to
  it. Field quoting, doubled quotes, NULL and empty-string fields, and a
  blank-padded `char(n)` field all match.
- `json_strip_nulls` answered NULL for every input, while `jsonb_strip_nulls`
  worked. sqlglot gives the non-`b` spelling its own node and leaves the `b`
  spelling an anonymous call, so the name-keyed dispatch served only one of
  them.
