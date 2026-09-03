### A tenth SQL sweep: a function that was legal at the top of a SELECT and nowhere else

DDL came back strong — 38 of 41 shapes already matched PostgreSQL 14.13, and
catalog introspection 15 of 22 — so this is a short batch. Three findings.

**A session function stopped being resolvable one level down.**
`current_setting('x')` worked; `current_setting('x') ~ '…'` answered
`42883 function current_setting(text) does not exist`. Those functions were only
ever reached from the constant-SELECT planner, so any operand position, `WHERE`
clause or wrapping call lost them — the same shape as a set-returning function
that only works as a row source, but here with no reason for the restriction.

**`has_table_privilege` ignored the owner.** It consulted recorded `GRANT`s
only, so a table the caller had just created and could plainly read reported
FALSE. PostgreSQL's owner holds every privilege implicitly — measured on 14.13
by creating a table, granting `SELECT` to another role, and asking as the
creator, which answers true.

#### Fixed

- **Session functions are legal wherever an expression is.** `current_setting`,
  `current_database`, `current_schema`, `current_query`, `version`,
  `pg_is_in_recovery` and the `inet_server_*` / `pg_postmaster_start_time` pair
  now resolve in any position. Deliberately *not* on that list: anything with a
  side effect (`set_config`, the advisory locks, `pg_terminate_backend`), which
  keeps its existing explicit handling. Widening where they are reachable does
  not widen what they accept — an unknown setting is still `42704`.
- **`has_table_privilege` grants the owner everything implicitly**, and honours
  a `REVOKE` that targets the owner (which materializes the ACL). This is the
  *reporting* function; the authz gate has its own path and already permitted
  the owner, which is exactly why the read worked while this denied it. The
  rule hands nothing to anyone else — a stranger still reports false.
- **`CREATE TABLE (id int, id int)` is rejected** with `42701 column "id"
  specified more than once`, instead of creating a relation whose second `id`
  was unreachable. Names fold, so `(id int, ID int)` collides too.
- **`ALTER TABLE ... DROP COLUMN`** names the relation in its 42703, as
  PostgreSQL does: `column "x" of relation "t" does not exist`.

An existing test asserted that the two-argument `has_table_privilege` was false
for the session user "(default 'secantus', no grant)". That is not the rule, and
PostgreSQL disagrees; the test now carries the measured value and the reason.
