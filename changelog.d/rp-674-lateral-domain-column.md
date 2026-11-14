### A LATERAL subquery's column over an outer domain column is of the domain

`select pg_typeof(b), array[b] from t x cross join lateral (select x.a b) l`,
where `t.a` is of a domain over `char(3)`, is now the domain and the
domain's array, with the padding, as on PostgreSQL. The subquery's scope
did not see the FROM items before it, so a column referring to one was of
no known domain and was typed by the domain's base. A subquery nested in
the LATERAL one sees that outer level too, and a name the subquery's own
FROM has still resolves there first.

#### Fixed

- The domain scope of a LATERAL subquery includes the outer FROM items.
  Fixes RP-674 (tests/backlog/test_rp_found_rp657.py::test_lateral_column_of_an_outer_domain_column_is_the_domains)
