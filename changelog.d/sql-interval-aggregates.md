### `sum(interval)` answered `0` and `avg(interval)` answered NULL

An interval rides as a subdocument, and Mongo's `$sum` over subdocuments is
`0` while its `$avg` is NULL — so both came back as **silently wrong data**
rather than an error. PostgreSQL gives `3 days` and `1 day 12:00:00`.

`min` / `max` were unaffected: Mongo's BSON order over the subdocument happens
to agree with duration order.

#### Fixed

- `sum(interval)` and `avg(interval)`, whole-table and grouped, on the join
  planners as well as the single-table ones, and whether the aggregate stands
  alone or sits inside a computed projection. NULLs are skipped and zero
  contributing rows is NULL, as every SQL aggregate is. The fold is
  componentwise for the sum (PostgreSQL's `interval_pl`) and carries months
  into days and days into micros for the average, which a per-field divide
  would get wrong.
- An interval **inside an array** rendered as its raw subdocument —
  `ARRAY[interval '1 day']::text` gave `{"{\"interval\": {\"months\": 0, …}}"}`
  where PostgreSQL gives `{"1 day"}`. The text cast defaulted every array
  element to `text` rather than inferring the element type from the values;
  the two shapes that carry real element identity (an inner array cast, an
  array of element casts) still take precedence.
