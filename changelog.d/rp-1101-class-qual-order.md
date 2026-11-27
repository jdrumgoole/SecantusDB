### Rust PG server: a join's class equality prints where its class began in EXPLAIN

A scan an equivalence class with a constant filters now lists that
class's equality where PostgreSQL does: among the scan's other class
equalities in the order `deconstruct_jointree` creates the classes (a
JOIN's ON before the WHERE, each clause in turn), after its non-class
quals. It was always moved last, so `select ... from e, k where e.a = k.a
and e.a = 3 and e.b = 'v3'` printed `Filter: ((b = 'v3'::text) AND (a =
3))` on `e` where PostgreSQL 15 prints `((a = 3) AND (b = 'v3'::text))`.

#### Fixed

- The order of a class constant's equality in a join scan's `Filter`,
  on the side the WHERE names and on the side the class gives it.
  Fixes RP-1101 (tests/backlog/test_rp_explain_costs.py::test_plan[a class constant's equality keeps its class's place among the quals])

#### Added

- RP-1147 filed: a class-constant join beside an expression qual on the
  fixed side stays a Hash Join.
