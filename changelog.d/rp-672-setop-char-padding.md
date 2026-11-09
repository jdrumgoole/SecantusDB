### Rust PG server: a `char(n)` value keeps its blanks through a set operation with no width

PostgreSQL keeps a `char(n)` value padded. A set operation whose sides
are a `char(n)` column and a `text`, a `varchar` or another width resolves
to `bpchar` with no modifier, and its rows carry each side's values as
they are. This server stores a `char(n)` value trimmed and pads it on the
way out to its column's width, so such a column had no width to pad to and
its values lost their blanks. Each `char(n)` side's values are now padded
to their own width where the set operation combines them, and the
combined column compares as `bpchar`, ignoring trailing blanks, for
UNION, INTERSECT, EXCEPT and ORDER BY.

#### Fixed

- `select a from t union all select 'e'::text` over a `char(3)` column
  `a` holding `'xy'` answers `xy ` with its blank, as on PostgreSQL 15.19,
  and so do an ARRAY subquery, a FROM subquery or a WITH over it
  (`octet_length` is 3). A `char(3)` domain column read the same way keeps
  its padding too.
- A `bpchar` value assigned to a `text`, `varchar` or `name` column drops
  its trailing blanks (`rtrim1`, `bpchar_name`): `insert into t(s) select
  'xy '::bpchar` stored `xy ` (3 bytes) in a `text` column, where
  PostgreSQL stores `xy`. The same holds for UPDATE.

#### Known gaps

- RP-693: `CREATE TABLE AS` of an unsized `bpchar` still stores its value
  trimmed.
- RP-692: INTERSECT returns the left side's value where PostgreSQL returns
  the one from the input it estimates smaller.

Fixes RP-672 (tests/backlog/test_rp_found_rp657.py::test_set_operation_of_char_domain_and_text_keeps_blanks)
