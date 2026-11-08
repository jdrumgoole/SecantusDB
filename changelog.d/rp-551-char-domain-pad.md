### Rust PG server: a cast to a domain applies its base type's modifier

`select 'ab'::dc3`, where `dc3` is a domain over `char(3)`, answered `ab`.
PostgreSQL 15.19 answers `ab `, blank-padded to the width. A cast to a
domain is a cast to its base type at the base's type modifier, and only
then are the domain's constraints checked (`coerce_to_domain`,
`getBaseTypeAndTypmod`). This server cast to the bare base type, so the
modifier was dropped. A padded column's whole row then compared false
against a record of the cast: `fc = row('ab '::dc3, 1)` was false where
PostgreSQL says true.

#### Fixed

- A cast to a domain pads, truncates or rounds to the base type's modifier,
  through a domain over a domain too, before the domain's CHECKs run:
  - `'ab'::dc3` is `ab `, and `'abcd'::dc3` is `abc`;
  - `'abcd'::dv3`, a `varchar(3)` domain with `check (value <> 'abcd')`, is
    `abc`. It was refused with 23514, because the CHECK saw the value
    before truncation;
  - `1.234::dn52`, a `numeric(5,2)` domain, is `1.23`, and `12345.6::dn52`
    is 22003 `numeric field overflow`;
  - a `timestamp(0)` domain rounds to the second.
- A column of a domain over a domain (`dc3n` over `dc3`) stores the innermost
  base type at its width. It pads to `ab `, and `'abcd'` is refused with
  22001 `value too long for type character(3)`. It used to be stored
  unpadded with no length check. The catalog still reports the column's own
  domain's modifier (-1), as PostgreSQL does.
- A padded domain value is a `char(n)` to operators and functions, so its
  trailing blanks do not count: `'ab'::dc3 = 'ab'` is true, and
  `('ab'::dc3)::text || '|'` is `ab|`.

Fixes RP-551 (tests/backlog/test_rp_found_rp529.py::test_char_domain_cast_pads,
tests/backlog/test_rp_found_rp529.py::test_domain_cast_applies_base_typmod),
with the crate test `tests::a_domain_over_char_n_coerces_to_its_base_width`.
Files RP-557 (a domain-typed result column is described with no type
modifier), RP-558 (a domain column's `atttypmod` is the domain's width) and
RP-559 (an array of a domain is described and printed as `varchar[]`).
