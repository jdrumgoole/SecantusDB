### `--payload random`, and the 19x disk finding it produced

`do-client` filled every document with a single repeated character. Both
engines compress their tables — zlib here, snappy in mongod — so that payload
compresses to almost nothing, and any measurement of bytes-on-disk becomes a
measurement of the compressor rather than the engine. `--payload random` fills
the same document shape with incompressible bytes instead. The default is
unchanged, so existing comparisons stay comparable.

Measuring with it immediately turned up something worth fixing: **SecantusDB
leaves 674 MB on disk where mongod leaves 41 MB** for the same 320 MB of
documents. The data files are not at fault — they are *smaller* than mongod's.
It is the write-ahead log: 639 MB against mongod's 11-16 MB.

The cause is the daemon's 2GB `--log-file-max` default. WiredTiger reclaims
only *completed* log files, so a workload writing 639 MB of WAL has produced
exactly one still-active file and nothing can be freed until 2 GB is reached,
where mongod's 100 MB files rotate and are removed after each checkpoint.
Setting `--log-file-max 128MB` drops the total 19x, to 35 MB — better than
mongod — and a separate experiment shows the 2GB default is worth under 1% of
throughput. The recommendation and its evidence are in `tasks/backlog.md`; the
default is deliberately left unchanged here, as a durability-adjacent default
deserves its own reviewed slice.

#### Added

- `--payload repeat|random` on `do-client`, defaulting to `repeat`.
