### Rust storage: counts and sizes no longer load whole collections

#### Fixed

- `count({})`, `collStats`, `dataSize`, `dbStats` and a tailable cursor's
  rollover check walk the collection's rows with a cursor, in constant
  memory. Before, each read every document's bytes into memory, and
  `dbStats` did it three times per collection, so on a large collection
  they could exhaust memory.

RM-191 part (Verify: none). Still open: these take time proportional to
the collection, where mongod reads its size-and-count metadata. The change
is in the shared `secantus-storage` crate.
