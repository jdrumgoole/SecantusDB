### Rust PostgreSQL server: an insert reads the heap only where PostgreSQL's B-tree insert does

When PostgreSQL inserts into a B-tree, it looks up row versions in the heap
only for entries with the same key (the unique check), and only for the
target leaf's entries when that leaf is full. The Rust PostgreSQL server
modelled that, but before each inserting command it computed the dead
versions of the whole heap. A run of one-row inserts was therefore quadratic
in the table's size. The heap is now read only for the entries the insert
can ask about. psycopg's `test_type_error_shadow` workload (150,000 prepared
UUID-keyed inserts over three connections) went from 33.7 s to between
15 and 22 s on a loaded debug build.

#### Changed

- `secantus-pg` (`btree_insert_versions`, `BTree::candidate_tids`): the dead
  versions an insert's unique check and simple and bottom-up deletion may
  remove are computed among the TIDs of the key's equal run and, when the
  leaf cannot take the command's new tuples, the leaf's (RP-014).
- `secantus-pg` (`BTree::check_unique`): the equal run is found by binary
  search in its leaf.
