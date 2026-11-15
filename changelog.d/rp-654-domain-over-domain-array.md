### A domain may be over another domain's array type

`create domain s as q[]`, where `q` is itself a domain, now creates the
domain, as PostgreSQL does, and `'{1,2}'::s` reads back as `{1,2}`. The
server knew a domain's base by a built-in name or a user type's name, never
by a user type's array, and refused it with 42704.

#### Fixed

- `CREATE DOMAIN` over a user domain's or enum's array type (`t[]`).
  Fixes RP-654 (tests/backlog/test_rp_found_rp607.py::test_domain_over_a_domain_array)
