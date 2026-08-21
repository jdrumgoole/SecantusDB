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

WiredTiger's own statistics then answered the question the whole investigation
had left open — why mongod's tail is better at the *same* cache size.
Normalised per GB of data written, **SecantusDB dirties 42x more cache bytes
than mongod**, along with 25x the application-thread disk reads. Dirty cache
bytes is exactly the quantity the earlier cache sweep proved governs the tail,
so that is the mechanism; it also explains why more cache helped, why smaller
documents helped more, and why no eviction knob or concurrency bound helped at
all. Where the 42x comes from is the next question, and it is written up with
candidates in `tasks/backlog.md`.

#### Fixed

- `--payload random` now varies per document, so a random dataset is actually
  incompressible across records rather than only within one.
