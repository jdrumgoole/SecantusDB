### A sorted `find` explains as SBE under `queryFramework: "sbe"`

A `find` sorted on one top-level field, ascending or descending and with
or without a limit, now explains and runs as the slot program mongod 8.2.11
runs it under a `queryFramework: "sbe"` query setting: a `sort` (top-k
under a limit) over a `project` of `getSortKeyAsc` / `getSortKeyDesc`,
with the `SORT` node's `queryPlan` and the sort stage's statistics. It was
explained in the classic shape.

#### Fixed

- Fixes RM-543 in part (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_sorted_find_is_mongods). An index-served `find`, a sort on two keys or a path, and a sort under a skip or a projection are still explained classic.
