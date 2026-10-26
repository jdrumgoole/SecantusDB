### The Rust PostgreSQL server: GIN and GiST indexes under SERIALIZABLE

A SERIALIZABLE read that PostgreSQL 15.19 answers through a GIN or GiST
index now reads through it here too. It locks the index pages PostgreSQL
locks, not the whole table. For GIN that is the metapage, plus the entry
leaf a key is on, or the root of its posting tree. For GiST it is every
page the scan visits. An insert checks the pages it changes. So two
transactions working on far-apart keys both commit, as they do in
PostgreSQL. All 31 permutations of PostgreSQL's `predicate-gin` isolation
spec now agree with PostgreSQL 15.19, and all 40 of `predicate-gist`.
Before, 19 and 22 did.

This rests on page-by-page models of the two access methods. Each is ported
from PostgreSQL's C source and checked against `pageinspect` on a private
15.19 cluster. Array columns now take the space PostgreSQL gives them on a
heap page, so their rows get PostgreSQL's `ctid`s.

#### Added

- `secantus-pg` `gin_index.rs`: GIN's pages, ported from PostgreSQL 15.19.
  - CREATE INDEX: `ginbuild` over the build accumulator, which keeps
    `ginbulk.c`'s memory count against `maintenance_work_mem`.
  - The entry tree, split by data size (`ginentrypage.c`).
  - Posting trees and their compressed segments (`gindatapage.c`,
    `ginpostinglist.c`): build-time packing, the 50/50 and 75% splits, and
    segment repacking.
  - The B-tree machinery both trees share (`ginbtree.c`).
  - `fastupdate`'s pending list and its cleanup (`ginfast.c`).
  - Free pages come back through the free space map (`GinNewBuffer`).
  - Tests check builds and one-at-a-time inserts, with `fastupdate` on and
    off, block by block against PostgreSQL's pages.
- `secantus-pg` `gist_index.rs`: GiST's pages for `point_ops` and
  `box_ops`.
  - An insert takes the subtree of least penalty (`gistchoose`), widens
    downlinks on the way down (`gistgetadjusted`), and splits pages with
    the double-sorting picksplit (`gist_box_picksplit`, through `gistSplit`).
    It inserts the downlinks a split makes (`gistfinishsplit`), and root
    splits are handled too.
  - `point_ops` builds sorted by Z-order (`gist_indexsortbuild`); `box_ops`
    builds one insert at a time, leaving the fill factor free.
  - A scan takes its pages in `gistgettuple`'s queue order, a ported
    pairing heap.
  - PostgreSQL's quicksort is ported too (`pg_qsort`), because picksplit's
    order of equal elements depends on it.
  - Tests check both build paths against PostgreSQL's pages.
- `secantus-pg` `am_ssi.rs`: each modelled GIN or GiST index keeps its model
  from CREATE INDEX on, and inserts, REINDEX, TRUNCATE and VACUUM change
  it. SSI scans read through these models.
  - A SELECT reports its residual WHERE and its LIMIT to the scan.
  - The scan prices a GIN or GiST path for the residual's quals.
  - It locks the index pages the scan reads, then each heap tuple it
    fetches: one heap page at a time for a bitmap scan, in index order for
    a plain one, up to the row that completes a LIMIT.
- `secantus-pgplan` `costsize`: `gincostestimate`, `gistcostestimate`
  (`genericcostestimate`), and the bitmap heap and plain index scans over
  them. With no statistics, `arraycontsel` gives the selectivity of `@>`,
  `&&` and `<@`, and `positionsel`, `areasel` and `contsel` give the
  geometric operators'. Tests check these against `EXPLAIN` on 15.19.

#### Fixed

- `secantus-pg`: an array's heap tuple was sized as if its elements were
  text. A `{1}` of `int4[]` took 64 bytes, where PostgreSQL's takes 56. It
  now takes `construct_md_array`'s size, and the planner's heap-page
  estimate includes array columns.

Fixes RP-254 (`tests/backlog/test_rp_isolation.py::test_ssi_index_access_methods`).
