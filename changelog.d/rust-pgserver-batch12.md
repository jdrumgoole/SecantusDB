### The Rust PostgreSQL server: rules as a real rewriter, a DELETE USING that deleted every row, overload choice by type, and HAVING without GROUP BY

Batch 12 started from the re-measured backlog, as batch 11 did: every open
Rust-server entry was probed against PostgreSQL 15. Two findings were silent
wrong answers, and both are fixed. Most of the rest of the batch replaces
approximations with PostgreSQL's own rules, transcribed from its source or its
catalogs: the rewriter for rules, `func_select_candidate` for choosing a
function overload, and `pg_proc` / `pg_type` for types. Each change is
measured against PostgreSQL 15.

#### Fixed

- **`DELETE ... USING (subquery)` deleted every row.** A derived table inside
  the correlated subquery that `USING` is rewritten to was not recognised as
  local, so the correlation never bound and the predicate matched every row.
- **`HAVING` without `GROUP BY` was ignored.** `select 1 from t having
  count(*) > 5` returned every row of `t`. The whole input is now one group,
  so the query yields one row or none.
- **Rules ran as per-row triggers.** A `DO ALSO` action ran once per row, and
  an `UPDATE` rule's action ran after the update and saw the new rows. Rules
  now rewrite the statement as PostgreSQL's `rewriteHandler` does: once per
  statement, actions before the original for `UPDATE` / `DELETE`, conditional
  `INSTEAD` rules negated into the original, and recursive rules refused
  (`42P17`).
- **Function results had the wrong type.** `round` / `ceil` / `floor` /
  `trunc` of an integer are `double precision`, as PostgreSQL picks the
  `float8` overload (its category's preferred type). `abs(real)` is `real`.
  Over a table column these came back as `numeric`.
- `generate_series(smallint, smallint [, smallint])` is `42725 ... is not
  unique`, as on PostgreSQL. A bigint series is `bigint`, and `select from
  generate_series(...)` is rows of no columns.
- A 2-D `varchar[]` / `bpchar[]` / `name[]` rendered as a 1-D array of the
  sub-arrays' text.
- `inet[]` / `cidr[]` to text prints each element as `inet_out` does, dropping
  a full host mask.
- `aclitem` resolves roles made with `CREATE ROLE`. `aclitem::oid` is
  `42846`, and `oid` has no arithmetic operators (`42883`).
- Under `LATIN1` / `LATIN9`, a binary text array is transcoded element by
  element. A column name the encoding cannot hold is `22P05`.
- Date and time input errors carry their position, and a month or day
  overflow carries PostgreSQL's `datestyle` hint. `to_date` has
  PostgreSQL's DETAIL lines and refuses an invalid day name. jsonpath
  `keyvalue()` ids follow the position of the value.

#### Added

- **Functions resolve by argument type** against PostgreSQL 15's `pg_proc`
  signatures and implicit casts: a call no overload takes is `42883`, and a
  call's result types the expressions around it.
- **Expression typing** covers CASE, COALESCE, NULLIF, GREATEST / LEAST, `||`,
  date and numeric arithmetic and scalar subqueries. Compared literals are
  coerced when the statement is analysed.
- **Aggregates with no FROM** (`select sum(1)`, `select count(*) having
  false`).
- **EXPLAIN** prints `Index Cond`, `Filter`, `Hash Cond`, `Join Filter` and
  scan aliases.
- **`CREATE TYPE`** takes `internallength`, `passedbyvalue`, `alignment`,
  `storage`, `category`, `receive` / `send` and the rest, with PostgreSQL's
  validation. `pg_type` carries the physical and I/O columns and array rows
  for the built-ins.
- **`information_schema`** lists the `pg_catalog` and `information_schema`
  relations and columns.
- Corpora `builtin_overloads`, `assign_types`, `explain_quals`,
  `create_type_options`, `infoschema_system`, `func_types` and `b12_residuals`,
  all at 0 divergences against PostgreSQL 15.
- The vendored `pgwire` fork's two TLS unit tests generate their certificate
  per run. They used to fail on a key file that the repository's `*.key`
  ignore rule keeps out.
