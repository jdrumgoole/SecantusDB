### Rust PostgreSQL server: VACUUM, HOT updates, pruning and the free space map

The heap now keeps PostgreSQL 15.19's page state beside each row's `ctid`.
Each page records its line pointers, and a page's tuples are tracked once a
delete or update touches it. The table has a free space map and a target
block for each backend. A row's place after a DELETE, an UPDATE or a VACUUM
is the one PostgreSQL gives it.

#### Fixed

- Fixes most of RP-193
  (`tests/backlog/test_rp_batch46.py::test_scenario[VACUUM frees a dead tuple's place]`
  and `tests/backlog/test_rp_batch48.py::test_scenario[...]`: `VACUUM leaves
  a HOT chain's root redirected`, `VACUUM frees a non-HOT version's line
  pointer for reuse`, `a full page is pruned when a scan reads it`, `a
  rolled-back delete keeps its row, a rolled-back insert's place is freed`,
  `VACUUM truncates the empty pages at the end`, `VACUUM with few dead items
  skips the index pass`, `a new backend finds space through the free space
  map`, `the free space map's upper levels wait for VACUUM`, `a running
  writer holds a dead tuple` and `a REPEATABLE READ snapshot holds a dead
  tuple`):
  - **HOT updates.** An UPDATE that changes no indexed column and fits on
    its old page makes a heap-only tuple of a HOT chain.
  - **Pruning on read.** A scan that meets a full or nearly full page prunes
    it (`heap_page_prune_opt`), so the page's dead tuples give their space
    back before the next UPDATE.
  - **VACUUM.** VACUUM removes what no snapshot can see:
    - it leaves a chain's root a redirect, and frees dead line pointers for
      reuse;
    - it skips the index pass when too few pages have dead items;
    - it records each page in the free space map and truncates empty pages
      at the end.
  - **The free space map.** The map is ported from `freespace.c` and
    `fsmpage.c`, its three-level tree and next-slot hint included. A new
    row goes where `RelationGetBufferForTuple` puts it: the backend's
    target block, else the map, else the last page.
  - **The removal horizon.** The horizon counts running transactions and
    the snapshots REPEATABLE READ and SERIALIZABLE keep.
  - **Deletes.** The store reports the rows a delete removes
    (`secantus_storage::set_delete_observer`; the shared storage crate,
    which the MongoDB server links but never sets it). A deleter's outcome
    is settled from the store once it is older than every snapshot still
    held.
- Fixes RP-195
  (`tests/backlog/test_rp_batch48.py::test_scenario[pg_relation_size counts the heap's pages]`):
  `pg_relation_size` was the rows' encoded bytes rounded to pages. It is now
  the heap's pages, as PostgreSQL's main fork is.

#### Known

- RP-193, what is left: the heap state is kept in the server process, so a
  restart loses the dead tuples, the free space map and the target blocks
  (`tests/backlog/test_rp_batch48.py::test_a_dead_tuple_outlives_a_restart`).
- RP-028 takes over the bitmap heap scan's order after a HOT update, which
  is a matter of the plan PostgreSQL picks
  (`tests/backlog/test_rp_batch47.py::test_scenario[a bitmap heap scan reads a HOT-updated row at its root]`).
  Its sentence on the heap's order after UPDATE or DELETE is gone:
  GROUP BY and DISTINCT over such a heap now come out in PostgreSQL's order.
- RP-196, filed: `pg_class` shows `relpages`, `reltuples` and
  `relallvisible` as 0, -1 and 0 whatever VACUUM counted
  (`tests/backlog/test_rp_batch48.py::test_scenario[pg_class shows what VACUUM counted]`).
