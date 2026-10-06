### The Rust servers: ordered cursors in bounded memory, cheaper primary-key reads, fewer READ COMMITTED replays, and a SIGTERM fix for `secantusd-rs`

Inside a transaction block, a cursor or portal over one table with an
`ORDER BY` of stored columns no longer holds its whole result in memory. A
primary-key read is about 3 µs faster. A READ COMMITTED block that reads a
table it has written no longer replays its writes when only other tables
changed.

#### Changed

- Rust PostgreSQL server: a `DECLARE CURSOR`, or an extended-protocol portal
  inside a block, over one table with an `ORDER BY` of stored columns is
  sorted at its first fetch in bounded memory. It spills sorted runs to
  temporary files and merges them. Fetching 300,000 rows of 2 KB in a block
  grew the server by 47 MB, down from 2.7 GB.
- Rust PostgreSQL server: a read by primary-key equality runs on its tokio
  worker directly, without `block_in_place`'s hand-off. In a release build
  a primary-key read went from about 49 µs to 45 µs.
- Rust PostgreSQL server: in a READ COMMITTED block that has written, a
  plain read of tables the block wrote takes a fresh snapshot only when
  another session committed to one of those tables (or to a catalog). It
  used to move whenever anything had committed. Then it replayed the
  block's whole write set, which made a long block quadratic: 800
  insert-then-read pairs beside another table's writer took 52.7 s, and
  now take 3.2 s.

#### Fixed

- `secantusd-rs` (the Rust MongoDB server binary) ignored SIGTERM when its
  parent had left SIGTERM blocked. It also lost a SIGTERM sent the moment
  it printed its "listening on" line, because the stop handler was
  installed after that line. It now resets the signal mask and installs
  the handler before opening storage, as `secantusd-pg` does.
