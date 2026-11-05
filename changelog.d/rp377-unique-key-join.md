### Rust PG server: EXPLAIN prices a join on a unique key

`explain select * from e join k on e.a = k.a`, where `k.a` is k's primary
key, printed zeros. It now prints PostgreSQL 15.19's Hash Join
(`cost=27.50..45.14 rows=1000 width=14`), and the same holds with the other
side filtered (`where e.b = 'v3'`, 18.75..38.50).

#### Fixed

- A side unique on its one join key makes the join `inner_unique`, and
  PostgreSQL then also weighs a merge join reading the key's index
  presorted and a nested loop probing it. EXPLAIN now builds each side's
  pathlist (its scan, the whole index in key order, and the index and
  bitmap heap scans parameterized by the other side) and runs it through
  the join search's `add_paths_general` with no hash path. The Hash Join
  is printed only when it beats all of those beyond `add_path`'s fuzz
  factor. The two merge totals this computes are PostgreSQL's own (125.60
  and 68.10).
- Still printing zeros (RP-381): the indexed side filtered by its own
  WHERE, a LEFT join, and a join filter beside the key.

Fixes RP-377 (tests/backlog/test_rp_explain_costs.py::test_plan[a join on a key a B-tree leads with], tests/backlog/test_rp_explain_costs.py::test_plan[a join on a unique key with the other side filtered])
