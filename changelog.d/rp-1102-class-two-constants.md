### EXPLAIN of an inner join whose equivalence class holds two constants is the empty Result

`explain select ... from e join k on e.a = k.a where e.a = 3 and k.a = 4`
used to print a Hash Join of two filtered scans. On PostgreSQL the ON's
equality puts `e.a` and `k.a` in one equivalence class, the WHERE gives
that class two different constants, and
`generate_base_implied_equalities_const` folds `4 = 3` to a constant false
join clause, which makes the join dummy. PostgreSQL prints `Result` with
`One-Time Filter: false`, and so does the Rust PostgreSQL server now.

#### Fixed

- The Rust PostgreSQL server now builds classes from the column = column
  equalities of the WHERE and of every inner ON, through any number of
  joins. An int2, int4 or int8 column joins a class with another integer
  column, and a text column with text. Every `column = constant` in
  either place adds its constant to its column's class. Before, only one
  column's own constants in the WHERE were compared (RP-400). An equality
  whose sides are cast to a common type is a different class member on
  PostgreSQL too, and is left alone.
- Fixes RP-1102 (`tests/backlog/test_rp_explain_costs.py::test_plan[a class fixed to two constants is the empty result]`).

#### Found

- RP-1148: a LEFT JOIN whose strict WHERE makes it inner
  (`reduce_outer_joins`) and gives its class two constants is not yet
  the empty Result.
