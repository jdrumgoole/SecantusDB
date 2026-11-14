### `min` and `max` over a `char(n)` column describe no modifier

An aggregate's result carries no type modifier in PostgreSQL, so `min(c)`
over a `char(3)` column is described as `bpchar` with modifier -1, while its
value is still one of the column's blank-padded ones. The Rust PostgreSQL
server described the column's width instead; it now describes -1 and pads
the value itself.

#### Fixed

- `min` / `max` over a `char(n)` column, or a domain over one, describe
  modifier -1 in the RowDescription, as on PostgreSQL 15.19, and still
  return the padded value (`'xy '`).
  Fixes RP-659 (tests/backlog/test_rp_found_rp557.py::test_min_max_over_char_n_has_no_modifier)
