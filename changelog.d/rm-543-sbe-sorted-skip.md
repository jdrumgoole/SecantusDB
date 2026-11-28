### Rust MongoDB server: a sorted `find` with a skip explains as SBE under `queryFramework: "sbe"`

When a query setting puts a `find` in SBE, a sort on top-level fields with
a skip now explains as the slot program mongod 8.2.11 runs. Before, it was
explained in the classic shape.

#### Fixed

- The skip is a `limitskip none` over the `sort`. It is its own `SKIP`
  node, and its constant is bound in the slot after the sort's. The
  `queryPlan`, the slot program and `executionStats` match mongod's. A sort
  with a skip and a limit both is still explained classically (RM-543 stays
  open for it).

Fixes RM-543 in part (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_sorted_find_is_mongods).
