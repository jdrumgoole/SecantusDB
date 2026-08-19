### Declared column widths are enforced, and a cast to text works in a WHERE

A `varchar(3)` column would accept `'abcd'` and store it. The declared width was
recorded, reported to clients, and used when reflecting the schema — but never
actually applied, so a column could hold data that contradicted its own
definition, and the value came back out at full length. Over-length input is now
refused with `22001 value too long for type character varying(3)`, on both
`INSERT` and `UPDATE`. Postgres' one exception is preserved: an overflow made up
only of trailing spaces is trimmed to fit rather than rejected, so `'abc  '`
still lands in a `varchar(3)` as `'abc'`.

The second fix is a query that quietly returned the wrong answer. `WHERE
n::text = '2'` found nothing, even for a row where `n` is 2. The cast was
applied when a value was on its way out to the client, but not when it was used
in a predicate — there, the *stored number* was compared against the string, so
nothing ever matched. Such comparisons are now evaluated per row, where the cast
is applied properly.

That surfaced a related trap worth knowing about: `numeric` values are stored in
a decimal form that the conversion did not recognise, so `WHERE d::text =
'2.50'` failed for its own separate reason even after the first repair. Both are
fixed, and `2.50` still renders as `2.50` — the declared scale survives, as it
does in Postgres.

#### Fixed

- `char(n)` / `varchar(n)` reject an over-length value with `22001` instead of
  storing it, on `INSERT` and `UPDATE`; a trailing-blank-only overflow is
  trimmed, as Postgres does.
- A comparison against a cast to `text` (`WHERE n::text = '2'`) matches the rows
  Postgres matches, including for `numeric` columns.
