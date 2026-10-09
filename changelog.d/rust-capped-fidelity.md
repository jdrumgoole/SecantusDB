### Capped collections on the Rust MongoDB server follow mongod in six more places

A probe of capped-collection behaviour against `mongod` 8.2.11
(`tools/probes/capped_collections.py`) found the Rust MongoDB server different
on 67 of 97 results. It now differs on none. Two of the causes lost or
over-kept data. An upsert was never held to the cap, so a `max: 3` collection
that received upserts grew without limit. And `max: 0`, which means "no
document limit" on `mongod`, was read as a limit of zero: every insert
evicted everything except the newest document.

Not covered by that probe, and still different: `convertToCapped` is not
implemented (the server answers `CommandNotFound`), and after a refused
statement aborts a transaction the server words the follow-up error
differently from `mongod`. The Python MongoDB server differs on 51 of the 97
and was not changed.

#### Fixed

- An upsert into a capped collection (`update`, `findAndModify`, a
  replacement, several in one command) evicts the oldest documents to stay
  within `size` and `max`, as an insert does.
- `max` of zero or less on a capped collection means no document limit. It is
  stored and reported as 2147483647, as on `mongod`. A collection created
  earlier with `max: 0` is no longer emptied on insert.
- `create` refuses a capped `size` under 1 or over 1 PB and a `max` of 2^31 or
  more, with `mongod`'s messages. They were stored as given, and a `max` over
  2^31 wrapped to a negative number.
- `size` and `max` are stored and reported as integers. pymongo sends `size`
  as a double, and `listCollections` echoed `1000.0`.
- `collMod` with `cappedSize` or `cappedMax` re-bounds the collection, checks
  the same ranges, and answers `InvalidOptions` for a collection that is not
  capped. Both options were accepted and ignored.
- `collStats` reports `max` and `maxSize` for a capped collection.
- A write to a capped collection inside a multi-document transaction is
  refused and the transaction aborted: `insert` and `update` report write
  error 263, `delete` write error 20, `findAndModify` fails with 263. They
  used to run.
