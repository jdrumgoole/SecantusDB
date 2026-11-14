### A COLLATE clause over a collated domain expression is answered

`select 'a'::d collate "C" < 'B'`, where `d` is a domain over `text`, used
to fail with 42804 `collations are not supported by type d`. PostgreSQL
allows a `COLLATE` over any expression of a collatable type, and a domain
over `text` is one. The Rust PostgreSQL server now asks the same question,
following a domain to its base type, so the comparison, a VALUES-list
comparison and an `ORDER BY` under the clause answer as PostgreSQL 15 does.

#### Fixed

- A `COLLATE` over an expression of a domain type is no longer 42804.
  Fixes RP-802 (tests/backlog/test_rp_found_rp796.py::test_collate_over_a_collated_domain_expression)
