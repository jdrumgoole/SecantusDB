### Rust PG server: `oidvector`'s comparison functions answer, called by name

`btoidvectorcmp('1 2', '3')` is 1 on PostgreSQL 15.19, and `oidvectorlt`
.. `oidvectorne` answer as the operators they stand behind. Here each was
refused with `function ... is not supported yet`. Behind that was a wrong
catalog: the copy of PostgreSQL's `pg_proc` the server resolves calls
against spelled every `oidvector` and `int2vector` argument as `oid[]` and
`int2[]`. So an untyped literal passed to one of those functions was read
as an array literal, and `'1 2'` was 22P02 `malformed array literal`.

#### Fixed

- `btoidvectorcmp` is computed as `oid.c` computes it. When the lengths
  differ it answers the difference of the lengths, so the shorter vector
  comes first. Otherwise it answers -1 or 1 at the first oid that differs,
  compared unsigned.
- `oidvectoreq`, `oidvectorne`, `oidvectorlt`, `oidvectorle`,
  `oidvectorgt` and `oidvectorge` are rewritten into the operator each
  stands for, which already orders an `oidvector` as `btoidvectorcmp`
  does (RP-603).
- The signatures of the 18 vector functions (`btoidvectorcmp`,
  `hashoidvector`, `int2vectorout`, `oidvectortypes`, ...) and the six
  `oidvector` operators now name `oidvector` / `int2vector`, as
  PostgreSQL's catalog does.

Fixes RP-612 (tests/backlog/test_rp_found_rp603.py::test_oidvector_comparison_functions).
The crate regression test is `secantus-pgplan`'s
`tests::btoidvectorcmp_orders_by_length_then_unsigned_oid`. Files RP-957:
`hashoidvector`, `hashoidvectorextended` and `oidvectortypes` are still
refused.
