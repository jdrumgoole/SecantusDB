### The sqllogictest gauge grows a second protocol lane — and catches a wire bug doing it

`invoke validate-slt` now runs every corpus file through **both** PostgreSQL
wire protocols: sqllogictest-rs's `postgres` engine (simple query) and
`postgres-extended` (Parse/Bind/Execute), completing the two-lane design the
gauge plan called for. 52 of 60 lane-files pass; the only failures are the
four declared SQLite-vs-Postgres divergences, doubled across lanes.

The new lane immediately earned its keep: a `SELECT` from a view over the
extended protocol answered Describe with NoData and then sent DataRows — a
protocol violation strict libpq clients reject outright. Describe now
expands view references (on a copy, leaving the stored prepared statement
pristine) so the declared row shape always precedes the rows.

#### Added

- `slt_validation/`: the `postgres-extended` lane (both engines per include
  file, lane-tagged report).

#### Fixed

- `sql/engine.py`: extended-protocol Describe of a SELECT-from-view
  answered NoData while Execute emitted DataRows.
