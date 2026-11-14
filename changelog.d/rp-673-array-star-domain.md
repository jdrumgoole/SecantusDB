### `ARRAY(select * ...)` over a domain column is the domain's array

An `ARRAY(...)` or scalar subquery whose target is a `*` standing for one
column of a domain now has the domain's type, as the same subquery naming
the column does: `select array(select * from (select a from t) s)` over a
`char(3)` domain column is that domain's array, with its padding.

#### Fixed

- A lone `*` target over one domain column is cast to the domain like a
  bare column reference.
  Fixes RP-673 (tests/backlog/test_rp_found_rp657.py::test_array_of_star_over_a_domain_subquery_is_the_domains)
