### Rust PG server: a domain's NOT NULL and CHECK errors name it as PostgreSQL does

The errors a domain's constraints raise now name the domain the way
PostgreSQL's `CoerceToDomain` does: the domain the value is coerced to,
printed by `format_type_be`. Each part of the name is quoted when
`quote_identifier` would quote it, and the name is qualified only when its
schema is off the search path. Before, the stored name was printed as it
was, so a mixed-case domain lost its quotes. A cast to a domain over a
domain, an array of one, or a column of one named the inner domain whose
constraint failed rather than the one cast to.

#### Fixed

- An INSERT of NULL into a column of the domain `"Mx839"`, and `select
  null::"Mx839"`, are 23502 `domain "Mx839" does not allow null values`,
  as on PostgreSQL 15.19. A value its CHECK refuses is 23514 `value for
  domain "Mx839" violates check constraint "Mx839_check"`. A keyword is
  quoted too: `select 0::s845."int"` names `s845."int"`.
- With `"Over845"` a domain over `"Mx839"`, `select 0::"Over845"`,
  `array[0]::"Over845"[]` and an INSERT of 0 into a column of
  `"Over845"` are 23514 `value for domain "Over845" violates check
  constraint "Mx839_check"`, naming the outer domain as 15.19 does, as
  are `'no'::dvv687` and `'no'::text::dvv687` over `dcc687`.

Fixes RP-845 (tests/backlog/test_rp_found_rp839.py::test_a_domain_error_quotes_a_name_that_needs_it, tests/backlog/test_rp_found_rp839.py::test_a_domain_over_a_domain_names_the_outer_one_quoted)

Fixes RP-696 (tests/backlog/test_rp_found_rp687.py::test_cast_to_a_domain_over_a_domain_names_the_outer_domain)
