### A function of a column, and the error that hid the gap

`regexp_replace(statement, 'prepare _pg3_\d+ as ', '', 'i')` in a select list —
how psycopg reads back its own prepared statements — failed with *"column
"name" must appear in the GROUP BY clause"*. The gap was real but the error was
wrong twice over: the router treated **any** function call in a target list as
an aggregate, sent the statement to the aggregate planner, and the planner's
refusal came out wearing a grouping error's clothes.

The router now names the aggregates it means (`count`, `sum`, `avg`, `min`,
`max`), and a scalar call over a column is a real computed column: the value is
worked out per row by the executor, and the type is fixed at plan time — the
describe pass sees no rows and has to name the column's type anyway. This is
the same machinery the catalog work built for cast chains, widened from casts
to calls.

`regexp_replace` itself came with it, with PostgreSQL's rules rather than a
regex library's defaults: only the **first** match is replaced unless the `g`
flag is given, `\1` references capture groups, `i` folds case, and a malformed
pattern is `2201B` — its own error class, because the pattern is broken, not
the value being matched. And `current_setting(NULL)` answers NULL rather than
refusing the argument.

#### Added

- Scalar calls over columns in a table select list, typed at plan time.
- `regexp_replace`, constant or over a column, with `g` / `i` flags and group
  references.

`pg_typeof` now answers a real regtype value rather than a display-name
string, so `pg_typeof(x)::oid` reads the type's oid and `::text` its name —
psycopg's wrapper tests read the oid form for every numeric wrapper, and a name
is not a number. And a range array carries its own oid (`int4range[]` is 3905,
not `varchar`), so a client builds Range objects from it in either format.

#### Fixed

- Any function call in a select list was routed to the aggregate planner, so a
  plain gap surfaced as a grouping error.
- `current_setting(NULL)` refused the argument instead of answering NULL.
- `pg_typeof(x)::oid` failed trying to parse a type name as a number.
- A range array was described as `varchar`.
