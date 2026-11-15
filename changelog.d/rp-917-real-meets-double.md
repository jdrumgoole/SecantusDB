### A real meets a double precision without a cast

PostgreSQL compares and adds a `real` and a `double precision` with cross-type operators of its own (`float48ne`, `float48pl`, ...), so a `real` column meeting a double, a numeric or an integer is taken as it is and only the other side is cast. The Rust PostgreSQL server's EXPLAIN and view definitions printed the real side cast to `double precision` first; they now print the operands as PostgreSQL does.

#### Fixed

- A `real` operand of a comparison or arithmetic over `double precision` is printed uncast.
  Fixes RP-917 (tests/backlog/test_rp_explain_costs.py::test_plan[comparison of real and double precision])
