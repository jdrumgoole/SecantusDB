### `oidvector`'s hash functions and `oidvectortypes` run

`hashoidvector`, `hashoidvectorextended` and `oidvectortypes` now answer
as on PostgreSQL. `oidvectortypes(proargtypes)` writes a function's
argument types, `integer, text`. They were refused as not supported.

#### Fixed

- `hashoidvector` is `hash_any` over the vector's oids as they sit in
  memory (not `hash_array`, as the backlog item first said), and
  `oidvectortypes` is `format_type` of each oid joined by `, `.
- `format_type(0, NULL)` is `-`, not `pg_node_tree[]`, and an oid no type
  has is `???`, not `???(99999)`, as PostgreSQL's
  `FORMAT_TYPE_ALLOW_INVALID` writes them.
- Fixes RP-957 (tests/backlog/test_rp_found_rp612.py::test_oidvector_hash_and_types)
