### The unnamed portal is not listed in pg_cursors

A parameterised statement sent through the extended protocol inside a
transaction no longer leaves a `pg_cursors` row named
`POSTGRESQL_DEFAULT_NAME`: like PostgreSQL, the server creates the unnamed
portal invisible, so only named portals and declared cursors are listed.

#### Fixed

- `pg_cursors` omits the extended protocol's unnamed portal.
  Fixes RP-936 (tests/backlog/test_rp_found_rp900.py::test_the_unnamed_portal_is_not_in_pg_cursors)
