### A missing GROUP BY column is reported where it is written

Under `from fs x` the relation's own name `fs` names nothing, so `group by
fs` is a missing column. The Rust PostgreSQL server reported it at the FROM
item, where a search for the name found it first; it now reports it at the
GROUP BY reference, as PostgreSQL 15.19 does.

#### Fixed

- `select count(*) from fs x group by fs` is 42703 `column "fs" does not
  exist` at position 36, the reference, not 22.
  Fixes RP-537 (tests/backlog/test_rp_found_rp532.py::test_alias_hides_relation_name)
