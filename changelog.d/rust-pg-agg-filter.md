### The Rust PostgreSQL server supports FILTER on an aggregate

`count(*) FILTER (WHERE n > 8)` was `0A000 FILTER on an aggregate is not
supported yet`. Only the rows matching the filter now contribute, and a group
where none match is the empty input — `count` is 0 and every other aggregate
NULL, which is what PostgreSQL answers.

Each aggregate carries its own filter, so `count(*) filter (where n > 8),
count(*)` computes both from one pass over the group, and a filter works
inside HAVING and beside DISTINCT.

#### Added

- `crates/secantus-pgplan`: `AggItem.filter`, lowered with the same
  WHERE-lowering the rest of the planner uses.
- `crates/secantus-pgserver`: `compute_aggregate` applies it.

#### Testing

- `tests/test_rust_pgserver_slice.py`: grouped and ungrouped, a filter that
  matches nothing, two aggregates with different filters, one inside HAVING,
  and one beside `count(DISTINCT ...)` — measured against PostgreSQL 14.24.
