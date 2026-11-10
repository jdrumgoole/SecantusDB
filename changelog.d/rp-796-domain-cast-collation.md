### Rust PG server: a cast to a collated domain compares by the domain's collation

On PostgreSQL, a value cast to a domain declared with `COLLATE` carries that
collation, so `'a'::d < 'B'::d` is true over a domain collated
`"und-x-icu"`. The Rust PostgreSQL server compared such a value bytewise and
answered false. The same held for `ORDER BY`, `max`, `min`, `greatest`,
`least` and `BETWEEN` over the cast, and for a VALUES list or subquery
column built from it. Two different domain collations meeting in one
comparison raised no error. The server now derives a cast's collation by
PostgreSQL's rules.

#### Fixed

- A cast to a domain whose `COLLATE` named a collation carries that
  collation as an implicit one, as `assign_collations_walker` gives a
  `CoerceToDomain`. A domain over such a domain inherits it. Comparisons,
  sorts and extremes over the cast now use it. Two different implicit
  collations meeting in a comparison are now 42P22 `could not determine
  which collation to use for string comparison`, as on PostgreSQL.
- A VALUES list or subquery column whose rows cast to one collated domain
  now takes that domain's collation.
- A cast written over a `COLLATE` clause, as in `('a' collate "C")::d` or
  `('a' collate "und-x-icu")::text`, keeps the clause as an explicit
  collation. PostgreSQL's `coerce_type` puts the coercion beneath the
  clause, so the clause wins over the domain's own collation. Before, the
  clause was lost and the cast compared bytewise.

Fixes RP-796 (tests/backlog/test_rp_found_rp782.py::test_a_cast_to_a_collated_domain_takes_its_collation).

Files RP-802 (a `COLLATE` over an expression of a collated domain is 42804)
and RP-803 (a VALUES or set-operation column whose rows carry two different
implicit collations sorts bytewise instead of having no collation).
