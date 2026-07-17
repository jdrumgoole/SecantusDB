### Expanded change-stream events now match mongod field-for-field

`showExpandedEvents` change streams now reproduce mongod 7.0.12's event
shapes exactly, on both servers. Expanded update events always carry
`disambiguatedPaths` — an empty document when nothing was ambiguous — and
plain streams never do. `dropIndexes` events describe the dropped index in
full (`{v, key, name}`, with the key spec captured at drop time), matching
`createIndexes`. And the Rust server now emits `dropIndexes` events on the
`dropIndexes: "*"` path at all — its `drop_all_indexes` previously skipped
the oplog, so `drop_indexes()` from a driver produced no event.

#### Fixed

- Expanded update events carry `disambiguatedPaths` (both servers); the key
  is correctly absent without `showExpandedEvents`.
- `dropIndexes` events describe the dropped index in full on both servers,
  and the Rust server emits them for `dropIndexes: "*"`.
