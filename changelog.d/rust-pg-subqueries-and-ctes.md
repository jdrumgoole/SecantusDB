### Subqueries and CTEs on the Rust PostgreSQL server

The Rust PostgreSQL server refused every form of subquery. `(SELECT ...)` as a
value, `EXISTS`, `IN (SELECT ...)`, a subquery in `FROM`, and `WITH` all
answered `0A000 ... is not supported yet`, which gated most real application
SQL — a table of gaps in `tasks/backlog.md` named them the biggest single
lever left on this server. Measured against a live PostgreSQL 14.13 with a
33-line corpus, 32 of 33 diverged.

They work now, in every uncorrelated form: scalar subqueries, `EXISTS` /
`NOT EXISTS`, `IN` / `NOT IN`, `ANY` / `ALL`, `ARRAY(SELECT ...)`, a subquery
in `FROM` with an alias and an optional column list, and non-recursive `WITH`
including several CTEs where one reads another. A widened 50-line corpus is at
five divergences, and all five are **correlated** subqueries — refused by name
(`a correlated subquery ...`), because a subquery whose value depends on the
outer row has no single set of values to substitute, and substituting one
would be a wrong answer rather than a missing feature.

Three bugs were found while building it, two of them wrong answers rather than
errors, and all three reachable before this change or made reachable by it.
The one that matters most is the correlation check itself: the lowering
resolves a column by the last part of its name and ignores the qualifier, so
`EXISTS (SELECT 1 FROM emp e WHERE e.dept_id = d.id)` bound the outer `d.id`
to `emp`'s own `id`, planned cleanly, and answered **true for every row**.

#### Added

- `secantus-pgplan` / `secantus-pgserver`: uncorrelated subqueries in every
  form. An uncorrelated subquery is evaluated once during planning and
  replaced by the values it returned — which is what PostgreSQL does with one
  too — so `IN (SELECT ...)` lowers through the same `ANY`/`ALL` path that
  already had SQL's three-valued rules right: an empty `ANY` matches nothing,
  an empty `ALL` matches everything, and a NULL in a `NOT IN` subquery makes
  the whole predicate NULL, so it returns no rows.
- `secantus-pgplan`: `FROM (SELECT ...) s` as a source in its own right, for
  plain and aggregate selects alike, with `s(a, b)` column aliases.
- `secantus-pgplan`: non-recursive `WITH`, rewritten into the FROM-subqueries
  it is shorthand for. `WITH RECURSIVE` and a data-modifying `WITH` are
  refused rather than inlined: the first has no subquery to expand into, and
  the second must run exactly once however many times it is referenced.
- `secantus-pgplan`: `21000` (cardinality_violation) for a scalar subquery
  that returns more than one row, as PostgreSQL reports it.

#### Fixed

- `secantus-pgplan`: a qualified aggregate argument read the FIRST part of the
  column's name, so `max(t.n)` answered `42703 column "t" does not exist` —
  over a plain table as much as over a subquery, and for as long as aggregates
  have existed.
- `secantus-pgserver`: an UNQUALIFIED column reference across a join whose
  left side is a subquery was assumed to belong to the right side, because a
  subquery side's columns could not be probed by name. They can now, from the
  side's own plan; before, `select x, y from (select 1 as x) a, (select 2 as
  y) b` answered `(NULL, 2)`.
- `secantus-pgserver`: a subquery runs during planning, which is outside the
  transaction scope the statement's execution runs in, so WiredTiger served it
  its own snapshot — `insert; select count(*) from t where id in (select id
  from t)` counted the rows from before the insert while the same query
  without a subquery counted correctly. The read now enters the open
  transaction.
- `secantus-pgplan`: `walk_column_refs` and the new subquery walk are one
  traversal rather than two over the same dozen node kinds — the drift the
  former's own comment was already warning about.
