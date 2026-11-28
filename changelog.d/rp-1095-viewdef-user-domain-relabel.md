### `pg_get_viewdef` relabels a user domain to its base type, as PostgreSQL does

A view over a column of a user domain now prints the way PostgreSQL 15
prints it. PostgreSQL resolves an operator or a function over a domain on
the domain's base type and relabels the operand to it, and ruleutils shows
that relabel: `where dd > 2` over `create domain dint as int` is
`WHERE ((u.dd)::integer > 2)`. The Rust PostgreSQL server typed such a
column by its base type and printed `WHERE (u.dd > 2)`.

#### Fixed

- A domain operand of an operator, a function or an aggregate, an `IN`
  list, `BETWEEN`, `IS DISTINCT FROM`, `NULLIF`, a row comparison or an
  `IN (SELECT ...)` prints with its relabel (`abs((u.dd)::integer)`,
  `(u.t)::text ~~ 'a%'::text`), through a domain over a domain to the
  last base.
- `COALESCE`, `GREATEST` / `LEAST` and `CASE` relabel a domain member
  when their arguments are not all that one domain, as
  `select_common_type` does, a CASE without ELSE counting its implicit
  `ELSE NULL` (`CASE WHEN ... THEN (u.dd)::integer ELSE NULL::integer
  END`).
- A domain is kept where PostgreSQL does not coerce it: the select list,
  ORDER BY, GROUP BY, `IS NULL`, `count(u.dd)`, a polymorphic or `"any"`
  argument (`array_agg(u.dd)`, `to_jsonb(u.dd)`, `lag(u.dd)`), and the
  arguments of a SQL-syntax call (`SUBSTRING(u.t FROM 2)`), which
  PostgreSQL prints without implicit coercions.
- A cast to a user domain prints as a `CoerceToDomain` (`(3)::dint`,
  `('a'::text)::dtxt`), and a cast to the domain the value already has
  prints no cast (`u.t`).
- An `information_schema` domain column now gets the same relabels in
  those positions; before, a view using one there printed as written.

Fixes RP-1095 (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_relabels_a_user_domain_operand);
the wider matrix of 98 views is tests/test_rust_pgserver_viewdef_domains.py.
Filed while working it: RP-1223, RP-1224, RP-1225, RP-1226.
