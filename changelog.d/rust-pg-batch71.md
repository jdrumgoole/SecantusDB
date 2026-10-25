### Rust PostgreSQL server: a B-tree deletes its dead versions before it splits

An index under repeated UPDATEs stays the size PostgreSQL 15.19's does: a
full leaf removes the versions dead to everyone before it splits. A table
outside `public` names its primary key as PostgreSQL does.

#### Fixed

- A B-tree leaf too full for a new tuple deletes before it deduplicates
  or splits, as `_bt_delete_or_dedup_one_page` does (part of RP-196):
  - **LP_DEAD marking.** A unique index's check (`_bt_check_unique`)
    marks LP_DEAD the equal keys whose every heap version is dead to all,
    from the leftmost leaf the key can be on.
  - **Simple deletion** (`_bt_simpledel_pass`) removes those tuples, and
    every other dead TID in their heap blocks or the new tuple's.
  - **Bottom-up deletion** (`_bt_bottomupdel_pass` with
    `heap_index_delete_tuples`) runs for an UPDATE that left the index's
    key unchanged, or a unique key already there. It works as PostgreSQL's
    does:
    - the TIDs in runs of equal keys are offered to the heap as promising;
    - at most six heap blocks are visited, the most promising first, with
      the free-space target halved past the favourable ones;
    - every TID whose HOT chain is dead goes;
    - deduplication is skipped when the page now has room.
  - **What it changes.** Five updates of every row of a 2,000-row table
    leave its primary key 17 pages, PostgreSQL's 17, where it was 24.
  - **How the heap is asked.** What is dead is read from the heap model
    once per command, its ended deleters settled first, before any B-tree
    is held.
- `REINDEX SCHEMA s` rebuilds every index of the schema's tables, where it
  rebuilt none. An unknown schema is 3F000. `REINDEX TABLE` and `REINDEX
  INDEX` take a schema-qualified name.

  Fixes part of RP-196 (tests/backlog/test_rp_batch60.py::test_scenario[bottom-up deletion keeps an unchanged key's index small],
  tests/backlog/test_rp_batch71.py::test_scenario[an unchanged non-unique key's index deletes its dead versions],
  tests/backlog/test_rp_batch71.py::test_scenario[a large primary key under repeated updates],
  tests/backlog/test_rp_batch71.py::test_scenario[an index whose key changes is not deleted from bottom-up],
  tests/backlog/test_rp_batch71.py::test_scenario[half the rows updated again and again],
  tests/backlog/test_rp_batch71.py::test_scenario[a text primary key under repeated updates]).
- A table outside `public` names its primary key after its own name:
  `create table s.t (a int primary key)` makes index `t_pkey` in schema
  `s`, where it made `s.t_pkey`, whose size read 0 (RP-249).

  Fixes RP-249 (tests/backlog/test_rp_batch71.py::test_scenario[a table outside public names its primary key as PostgreSQL does]).

#### Known gaps

- An index scan marks no entry LP_DEAD (`kill_prior_tuple`), and one step
  of a long run of updates differs by a page (RP-196, open).
