### Rust PG server: DROP DOMAIN and DROP TYPE follow a type's dependents all the way down

A DROP of a domain that another domain was over dropped the base alone. It
left the second domain over a type that no longer existed. A DROP now walks
the dependents as PostgreSQL's `findDependentObjects` does, so a chain of
domains is refused, or cascaded to, whole.

#### Fixed

- `drop domain d` with a domain over `d` dropped `d` alone, and `drop
  domain d cascade` did not drop the domain over it. `drop type d cascade`
  dropped the dependent domain, then failed 42704 and left `d` in place.
  `drop type d` of a domain nothing depends on was 42704. Each now
  answers as PostgreSQL 15.19 does.
- The walk follows domains over domains, each with its own columns, and
  the columns and domains over a type's array type (`depends on type
  d[]`). A domain's columns, the fields of composite types over it, and
  the tables made `OF` it are followed too.
  - RESTRICT's 2BP01 DETAIL, and CASCADE's notice, list the dependents
    in PostgreSQL's order: depth first, newest first, in reverse of the
    order the walk finished them.
  - A DROP of an enum, composite or range reaches domains over those
    domains as well.
- A DROP of several types goes through one walk, as in
  `performMultipleDeletions`. One named type over another is not reported
  as a dependent. What depends on any of them is reported once, under
  `cannot drop desired object(s) because other objects depend on them`.
- `drop domain` naming an enum, composite or range type is 42809 `"e" is
  not a domain`, as on PostgreSQL. It was 42704 `does not exist`.

Fixes RP-607 (tests/backlog/test_rp_found_rp580.py::test_drop_of_a_domain_another_domain_is_over,
tests/backlog/test_rp_found_rp580.py::test_drop_walks_a_types_dependents).
