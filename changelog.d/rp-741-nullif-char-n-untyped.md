### Rust PG server: NULLIF of a char(n) against an untyped literal keeps its type and padding

`nullif('ab'::char(4), 'zz')` is now `character` `ab  `, as on PostgreSQL
15.19, where it was `text` `ab`. PostgreSQL's operator resolution reads an
untyped literal or a bare NULL as the other operand's type, so the `=` is
`bpchareq` and the result is the left operand as it is. NULLIF read that
side as `text`, so it resolved to `texteq`, cast the left operand to text
and dropped its padding. Both the described type and the value now read
the untyped side as the left operand's type, so `nullif('a'::varchar, 'b')`
still resolves through `texteq` to `text`, as PostgreSQL does.

#### Fixed

- `nullif(c, 'zz')` and `nullif(c, null)` over a `char(n)` value describe
  `character` and keep its trailing blanks (`octet_length` counts them).

Fixes RP-741 (tests/backlog/test_rp_found_rp737.py::test_nullif_char_n_against_untyped_keeps_bpchar)
