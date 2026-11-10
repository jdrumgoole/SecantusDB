### Rust PG server: a window `min` / `max` over `inet` or `cidr` picks in network order

A window `min` / `max` over a network value compared the values' text, so
`min(x) over ()` over `10.0.0.1`, `9.0.0.0` and `::1` answered `10.0.0.1`
where PostgreSQL answers `9.0.0.0` (`network_cmp`: IPv4 first, then by
address). The plain aggregate already picked in network order.

#### Fixed

- `min` / `max` with OVER over `inet` or `cidr` pick by `network_cmp`,
  their window's ORDER BY keyed the same way, and answer the overload's
  type (`max(cidr)` is an `inet`).
- An ORDER BY naming a window target by position or alias is answered
  when the target's value needs no order key: `(max(x) over (order by
  x))::text ... order by 1` over a network value was 0A000 `function
  max(inet) is not supported yet`, and `row_number() over (order by v)
  ... order by 2` over a range or an `oidvector` was 0A000 `function
  row_number() is not supported yet`. The target was copied into the sort
  clause, where a window call cannot run.

Fixes RP-754 (tests/backlog/test_rp_found_rp706.py::test_a_window_min_max_over_a_network_value_uses_network_order).
Fixes RP-652 (tests/backlog/test_rp_found_rp611.py::test_order_by_a_window_target_over_a_keyed_window).
Files RP-761 (a window call in an ORDER BY key is 0A000), RP-762 (an ORDER BY
comparison over a network value compares the text) and RP-763 (a window
`min` / `max` under a collation picks by the text's bytes).
