### Set-returning functions work as join and derived-table sources

`generate_series`, `unnest` and friends worked only as the *sole* `FROM` item.
Used anywhere else — joined to a table, or inside a derived table — they
failed with `relation "" does not exist`, an error naming a relation nobody
had written. The empty name was the tell: sqlglot models a table function as
a table whose name lives in a function node rather than an identifier, so the
planner fell through to a catalog lookup for the empty string.

Such a source is now reduced to the base-less shape the engine already knows
how to materialize, and handed to the executor as a raw sub-plan. That matters
for more than tidiness: the rows are produced at execution time, so an SRF
whose arguments read session state — `generate_series(1,
array_upper(current_schemas(false), 1))` — resolves against the real session
instead of being guessed at while planning.

`pg_type` also gained `typinput`, the column drivers compare against
`array_in` to decide whether a type is an array.

Together these let the JDBC driver's type-lookup query run, which had been the
single largest source of failures in its conformance suite; the gauge moves
from 92.5% to 93.7%.

#### Added

- `pg_catalog.pg_type.typinput`.

#### Fixed

- A set-returning function in `JOIN` position, or in the body of a derived
  table, no longer fails with `relation "" does not exist`.
