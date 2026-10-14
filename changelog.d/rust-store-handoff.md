### A collated or geo index stays right when a store changes hands

Four silent wrong answers came from data already on disk. A geo index the
Rust MongoDB server built before 2026-10-13 had no entry for a value with
several locations, or for a point written as a document other than
`{lng, lat}`, so a query through it missed those documents. A collated index
it built before 2026-10-10 compared plain values. A collection with a default
collation created before 2026-10-11 had no claim for the `_id`s already in
it, so a second spelling of one was accepted. And the two MongoDB servers
key a collated or geo index differently, so a store handed from one to the
other answered from entries the reader could not read.

Each server now marks the collated and geo indexes it builds, and rebuilds
one without its own marker when it opens a store, from the options the user
gave and with no change-stream event. An index the stored documents no
longer allow (a unique collated index two of them now break) stops the open
and is named: the Rust server leaves it in place, the Python server drops it.
The Rust server claims every stored `_id` under its collection's collation
once per store; the Python server, which takes no claims, clears the record
of that walk when it opens a store, so the Rust server walks again after it.

#### Fixed

- `crates/secantus-storage`: `rebuild_outdated_indexes` and
  `backfill_collated_id_claims` at open; geo indexes carry `geoKeys`.
- `src/secantus/storage.py`: `_reconcile_store_from_rust_server` at open;
  collated and geo indexes carry `pythonKeys`.
- `listIndexes` on both servers leaves out `collationKeys`, `geoKeys` and
  `pythonKeys`. The Python server showed the Rust server's `collationKeys`.
- `tests/test_rust_store_handoff.py` hands one store Rust -> Python -> Rust.
