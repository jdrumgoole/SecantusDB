### `bulkWrite`, `sort` on updates, and an honest 8.x version

SecantusDB advertised itself as MongoDB 7.0 while its error surface had already
been retargeted to 8.x. That gap was not cosmetic: the advertised version is a
capability contract, and drivers gate features on it. `client.bulk_write()` —
MongoDB 8.0's command for writing across several collections at once — was
simply unavailable, and had a caller sent the command by hand they would have
got `CommandNotFound`.

Three things landed together, because they cannot land apart. The driver spec
suites assert in *both* directions: that a pre-8.0 server rejects `sort` on an
update, and that an 8.0 one honours it. Implementing without advertising fails
the first set; advertising without implementing fails the second.

#### Added

- **`bulkWrite`** — inserts, updates and deletes across multiple namespaces in
  one command, with MongoDB's cursor-shaped reply and summary counters. Ordered
  batches stop at the first failing operation and unordered ones continue;
  upserts report the generated `_id`; `errorsOnly` suppresses successful
  results. A failing operation is reported against that operation, leaving the
  rest of the batch to run — an early version let it fail the whole command, so
  a driver saw no partial result at all.
- **`sort` on `updateOne` / `replaceOne`** — matches in sort order and updates
  the first. Rejected in combination with `multi`, as MongoDB rejects it, and an
  upsert that matches nothing still upserts.
- **`nsType` on change-stream `create` events** — `collection`, `timeseries` or
  `view`.

#### Changed

- The advertised server version moves from 7.0.0 (wire 17) to **8.2.11 (wire
  27)**, matching the MongoDB the project targets and tests against.

Every shape was measured against a live MongoDB 8.2.11 rather than derived from
documentation: the command's reply layout, its five rejection codes, the
ordered/unordered split, and the three `nsType` values.
