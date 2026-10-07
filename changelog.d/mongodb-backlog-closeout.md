### Both MongoDB servers close out their backlog against mongod 8.2.11

Every open backlog item for the Rust and Python MongoDB servers was reproduced
against mongod 8.2.11 and fixed, closed by re-measurement, or given a written
disposition (`tasks/backlog.md` §7.05). Most of the work landed on the Python
reference server, which had fallen behind the Rust server on error replies,
write concern, failpoints, `maxTimeMS`, change streams and collation. On every
differential probe in `tools/probes/` the two servers now give the same answers
as mongod, except for differences that are documented and deliberate.

Several of the Python fixes were silent writes, where a client got an
acknowledgement for a result mongod would not produce:
- a failed `updateMany` rolled back the documents it had already rewritten;
- `$set: {"b.2.c": 1}` over `b: []` stored nothing;
- `multi: true` with a replacement applied it;
- `delete` with `limit: 5` deleted;
- `createIndexes` built a duplicate index under a new name, and built an index
  over an empty key;
- `a$b`, `""` and `system.foo` became collections;
- `$sort` tied `true` with `1`.

#### Fixed

- **Python server, writes:**
  - A multi-update that fails part-way keeps the documents written before the
    failure. The reply is still `n: 0`, as mongod reports it.
  - An intermediate array index past the end is created and padded with nulls.
  - A `multi` replacement is a per-statement 9.
  - A numeric delete `limit` other than 0 or 1 is 9.
  - Duplicate keys on `update` / upsert / `findAndModify` carry the executor
    prefix.
  - An `_id` changed by a replacement, `$rename` onto `_id`, and an unmatched
    positional `$` use mongod's words.
- **Python server, indexes:**
  - 85 / 86 index conflicts, in mongod's text.
  - An empty key, an unknown plugin and a missing name are refused with mongod's
    codes.
  - A unique build over duplicates fails the build with mongod's wrapper, and
    names the colliding key.
- **Python server, namespaces and commands:**
  - Namespace validation on `create` and on every write command.
  - `renameCollection` outside `admin` is 13.
  - `collMod` refuses unknown fields (40415).
  - `find` sort values follow the `$sort` stage's rule: a double truncates and a
    decimal rounds half to even.
  - An invalid regex answers 51091 with PCRE2's message.
  - `$sort` orders every number before every bool.
- **Python server, rules ported from the Rust server:**
  - Validation `errInfo`, and the validator checked at `create` / `collMod`.
  - The `writeConcern.w` rules.
  - The awaitable `hello`.
  - `failCommand` labels and code names (`mongod_codes.py`).
  - 388 for an oversized transaction.
  - `maxTimeMS` executor prefixes. The index-build envelope and the write
    prefixes are given only when running standalone, as mongod does.
  - PCRE `\Z` / `\z`.
  - Type-first document comparison.
  - `startOfWeek`.
  - Decimal `$range` / `$log` / `$pow` / `$bucketAuto`.
  - Negative `$slice`.
  - `$project: {_id: 1}`.
  - The 32-bit index-argument rules, including the `$range` memory limit (146).
- **Both servers:**
  - Collated ordering expands a compatibility character (`ﬁ`) at the primary
    and secondary levels.
  - `ß` sorts as `ss` plus a secondary weight. This also makes the PostgreSQL
    server's `ORDER BY ... COLLATE` agree with PostgreSQL on `Straße`.
- **Rust server:**
  - `renameCollection` checks argument types before the `admin` check.
  - `--block-compressor lz4|zlib|none` is exposed on the daemon.
- **Tooling:**
  - `detached_run.py` records the exit in its state file.
  - Gauge reports are stamped with the date of the raw artifact.
  - `max_time_expiry.py` covers `createIndexes` and follows mongod's topology.
  - Two load-sensitive tooling tests now poll.
