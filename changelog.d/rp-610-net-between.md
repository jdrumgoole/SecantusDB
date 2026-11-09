### Rust PG server: a network type's BETWEEN compares in `network_cmp` order

`'10.0.0.10'::inet BETWEEN '10.0.0.2' AND '10.0.0.20'` was false, and a
`WHERE a BETWEEN ...` over an `inet` or `cidr` column kept no row: the bounds
compared as text, which puts 10.0.0.10 before 10.0.0.2. PostgreSQL 15.19
answers true, ordering addresses by `network_cmp`.

#### Fixed

- A BETWEEN, NOT BETWEEN, BETWEEN SYMMETRIC or NOT BETWEEN SYMMETRIC over a
  network type expands into the comparisons `transformAExprBetween` builds,
  and each one compares by the network order key as `<` and `>=` already did.
  Each comparison resolves its own operand types, so a bound that is not a
  valid address is 22P02, as on PostgreSQL.
- A VALUES or subquery column cast to `inet` or `cidr` compares, sorts and
  aggregates as that type: `WHERE a < '10.0.0.2'` over one compared text.

Fixes RP-610 (tests/backlog/test_rp_found_rp603.py::test_network_between_orders_by_network_cmp)
