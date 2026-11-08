### Rust PG server: a cast of a column to a domain applies the domain's modifier

`select t::d from t570`, where `d` is a domain over `char(3)` and `t` a
`text` column holding `ab` and `abcd`, answered `ab` and `abcd` where
PostgreSQL 15.19 answers `ab ` and `abc`, and `ta::d[]` over a `text[]`
column came back as stored too. A cast of a constant or of an expression
already applied the modifier; a cast of a bare column, in a plain select,
a join or a FROM-subquery alike, took a shortcut that only noticed a
modifier written in the cast itself.

#### Fixed

- A cast of a column to a domain whose base type carries a modifier (or to
  that domain's array) coerces to the base type at the domain's modifier,
  as `coerce_to_domain` does: a `char(3)` domain pads and truncates, a
  `varchar(2)` one truncates, a `numeric(5,2)` one rounds. A domain over
  that domain checks its CHECK against the coerced value, so `abcd`
  truncated to `abc` fails a `value <> 'abc'` CHECK, as on PostgreSQL.

Fixes RP-580 (tests/backlog/test_rp_found_rp570.py::test_column_cast_to_domain_applies_typmod)

#### Found

- RP-607: `DROP DOMAIN` of a domain another domain is over drops it alone,
  with or without CASCADE, and `DROP TYPE ... CASCADE` of it fails
  part-way (tests/backlog/test_rp_found_rp580.py).
