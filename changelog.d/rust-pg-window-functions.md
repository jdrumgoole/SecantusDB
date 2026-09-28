### Window functions on the Rust PostgreSQL server

`sum(v) OVER (PARTITION BY g ORDER BY id)`, `row_number()`, `rank()`, `lag`,
`lead` — none of them worked, and most did not even fail honestly. Ten were
refused by name, but the seventeen most common shapes answered
`42803 column "id" must appear in the GROUP BY clause`: the planner matched
`sum` on its NAME, never looked at the `OVER` beside it, and routed the query
into the aggregate planner, which demanded a `GROUP BY` the query neither has
nor needs. The client got an error blaming its own SQL for a feature the
server did not have.

Measured against a live PostgreSQL 14.13, the existing window corpus diverged
on **27 of 27** lines. Across four corpora totalling 89 lines it is now 86,
and the three that differ are all refusals that name what is missing: a
window over an aggregate, over a JOIN, and over a generated source.

Three of those corpora were written for this change, and the third one earned
its place: the first two agreed with PostgreSQL on all 75 lines while a
`RANGE` frame whose bounds sat on ONE side of the current row — `RANGE
BETWEEN 1 FOLLOWING AND 20 FOLLOWING` — returned the whole partition on every
row. Neither corpus contained such a frame. A corpus that agrees completely
is evidence about the shapes it holds and nothing else.

The whole family works: `row_number` / `rank` / `dense_rank` /
`percent_rank` / `cume_dist` / `ntile`, `lag` / `lead` / `first_value` /
`last_value` / `nth_value`, and the aggregates as windows; `PARTITION BY` and
`ORDER BY` over columns or expressions; `ROWS`, `RANGE` and `GROUPS` frames
including value offsets; all three `EXCLUDE` forms; named `WINDOW` clauses;
and `FILTER`.

One behaviour is worth stating because it is the one an implementation
usually gets wrong: the DEFAULT frame is `RANGE BETWEEN UNBOUNDED PRECEDING
AND CURRENT ROW`, and under `RANGE` a bound at `CURRENT ROW` means the
current row *and its peers*. So two rows that tie on the `ORDER BY` get the
SAME running total, and a window with no `ORDER BY` at all sees the whole
partition — the second falls out of the first, because with nothing to order
by every row is a peer.

#### Added

- `secantus-pgplan` / `secantus-pgserver`: window functions, computed over
  the materialised rows after the `WHERE` and before `DISTINCT` / `ORDER BY` /
  `LIMIT`, which is PostgreSQL's evaluation order. Each lands in a synthetic
  `__winN` field the select list projects, the same way a computed `ORDER BY`
  key already used `__orderN`.
- The aggregate windows go through the ORDINARY aggregate accumulator rather
  than a second implementation, so `sum(int4)` widens to int8, `sum(numeric)`
  stays exact and `avg` divides as numeric exactly as a `GROUP BY` does.
- `22014` for `ntile(0)`, which PostgreSQL gives its own class rather than the
  generic `22023`.

#### Fixed

- `secantus-pgplan`: `ORDER BY` now sees the select list's output names, so
  `select id * 2 as d from t order by d` works. Independent of windows — it
  answered `42703 column "d" does not exist` for any aliased column — but it
  is what makes a window usable, since `ORDER BY rn` is the only way to sort
  by one.
- `secantus-pgplan`: a window's synthetic column reaches the def a subquery or
  CTE publishes, so `select rn from (select row_number() over (...) as rn from
  t) s` can name what the window computed.
- `secantus-pgserver`: the window columns are added to the def `select_docs`
  RETURNS rather than only in `select_def`. Every reader of a select's schema
  takes the former, and without it a window value went over the wire as TEXT
  while its value was right — which a row comparison alone does not catch.
- `secantus-pgplan`: a window function beside a `GROUP BY`, over a `JOIN`, or
  over a generated source is refused by that name, instead of the false
  `function sum() is not supported yet` / `function row_number() is not
  supported yet` those paths used to produce.
- `secantus-pgplan`: a window function in a `WHERE` answers PostgreSQL's own
  `42P20 window functions are not allowed in WHERE`, and a `RANGE` offset over
  more than one `ORDER BY` column answers `42P20` with PostgreSQL's wording.
