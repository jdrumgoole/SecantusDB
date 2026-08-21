### Fix `--payload random`, and find an uncompressed WAL

`do-client --payload random` generated one random payload per worker and reused
it for every document. Each document was incompressible on its own and every
document was identical — and WiredTiger compresses blocks spanning many
records, so it crushed them anyway: 20,000 "random" 8 KiB documents produced an
8.8 MB table. The option existed precisely to make storage measurements honest
and it was doing the opposite. The payload is now derived per document.

Re-measuring with it found something: **SecantusDB writes byte-identical WAL
volume for random and repeated payloads — 2.04x the logical data either way.**
That is proof the write-ahead log is uncompressed, where mongod defaults to
snappy. On compressible data the gap is 2.04x against 0.05x, roughly 40x the
write I/O for the same workload.

Enabling `compressor=zlib` on the log — zlib is already linked into the
vendored WiredTiger and is what the data tables use — cuts p99.9 latency by
22% for 5% of throughput. That is a much better trade than the admission
control prototype, and it composes with `--oplog-async`. The recommendation is
in `tasks/backlog.md`; no default is changed here.

WiredTiger's own statistics were then used to compare the two engines' cache
behaviour, which looked like it answered the question the investigation had
left open. It did not: the `bytes dirty in the cache cumulative` counter turns
out not to cover every table's writes — SecantusDB's documents table writes
12.4 GB from cache while that counter reads 0.24 GB — so any cross-engine
comparison built on it measures instrumentation coverage rather than work.
Both the "42x more dirty bytes than mongod" and the "oplog is 95% of dirty
bytes" findings are **retracted**, with the evidence for the retraction
recorded in `tasks/backlog.md`.

What survives is the counter that behaves consistently: `bytes written from
cache` shows the oplog and the documents table each writing ~1x the logical
data, so ~2x in total — which independently agrees with the 2.04x measured from
WAL file sizes. The cache *sweep* (tail versus cache size) is a black-box
measurement and is unaffected. Why mongod holds a better tail at the same cache
size remains unexplained.

#### Fixed

- `--payload random` now varies per document, so a random dataset is actually
  incompressible across records rather than only within one.
