### Rust PG server: GROUP BY, DISTINCT and PARTITION BY match a `char(n)` value without its trailing blanks

Grouping a `char(n)` value split it from the same value written without
padding. `select count(*) from (values ('ab'::char(3)), ('ab'::bpchar)) v(a)
group by a` answered two groups of 1. PostgreSQL 15.19 answers one group of
2, because `bpchar`'s equality (`bpchareq`) and its hash (`hashbpchar`)
ignore trailing blanks. The same split showed in `SELECT DISTINCT`,
`DISTINCT ON`, `count(DISTINCT ...)`, `array_agg(DISTINCT ...)` and a
window's `PARTITION BY`, and over `char(n)[]` values too. A stored
`char(n)` column was not affected, because its values are stored trimmed.
Values reached through VALUES, a cast or an expression keep their blanks.

All of these match rows through one identity function, which only treated
`jsonb` specially. It now takes the value's type and compares a `char(n)` or
`char(n)[]` value without its trailing blanks, through domains as well. As
in PostgreSQL, a group still prints its first row's value, blanks included.
A window's partition keys now carry their column types for the same check.

#### Fixed

- Fixes RP-725 (tests/backlog/test_rp_found_rp719.py::test_grouping_a_char_n_value_ignores_its_blanks).
  The other shapes, and `text` / `varchar` keeping their blanks, are pinned
  by tests/backlog/test_rp_found_rp719.py::test_distinct_and_partitions_match_a_char_n_value_without_its_blanks.
