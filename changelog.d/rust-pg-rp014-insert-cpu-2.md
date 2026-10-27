### The Rust PostgreSQL server: less CPU per INSERT, second pass

A run of single-row INSERTs costs the server less again, with every answer
and every page model (heap, B-tree, SSI, statistics, catalog-read
counters) unchanged. On `test_type_error_shadow`'s workload from
psycopg's suite (three connections, 50,000 INSERTs each, debug build), the
server retires about 19% fewer instructions than after the first pass
(RP-014).

#### Changed

- `secantus-storage` (linked by the MongoDB server too): an insert's oplog
  entry is written into its final bytes at once (its `ts` and `wall` too),
  and its `op` / `ns` are read off those bytes; an entry is searched for a
  retryable write's `rw` stamp only once one has been made in the process.
  A common `_id`'s `{_id}` document is written straight to bytes. The
  per-collection write locks and record-id counters are found by borrowed
  names, with no key built for one already there. Unit tests hold each
  to the bytes the old path wrote.
- `secantus-pg`, the B-tree model: a unique insert's check starts from the
  leaf its TID-less search found, as `_bt_findinsertloc` does, without a
  second descent; when no heap TID needs the heap's answer (a new key on
  a leaf with room) the insert goes in under the same hold of the tree as
  its candidate pass. Key comparisons of text and uuid keys skip the
  general comparison.
- `secantus-pg`, per statement: Describe answers from its memo without
  installing the session's state on the thread; an Execute of parameters
  Bind decoded installs it once (twice only for a statement that changes
  the catalogs, whose effects are read before the statement runs). The
  settings a statement reads several times over (`track_counts`, the
  timeouts, the role, read-only, the client encoding, the buffer pool's
  size) are read once per change of the session's settings. The plan and
  describe caches, and the planner's statement-text memos (privileges,
  table locks, catalog reads, parse trees), answer a repeated text by
  comparing it rather than hashing it. A row's xid and command id are
  taken under one hold, with no text round trip. Rows are moved into
  their stored form when nothing reads them after the insert. A Bind with
  no enum in the catalog does not list the enums for each parameter.
