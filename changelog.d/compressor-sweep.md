### The block compressor is the largest performance lever in the engine

Profiling put 65% of the daemon's CPU inside zlib's `deflate`, on WiredTiger's
page-reconciliation path. That prompted the obvious question nobody had asked:
zlib is not the only option, and MongoDB defaults to snappy.

WiredTiger can now be built with the snappy / lz4 / zstd builtin extensions
alongside zlib (`-DSECANTUS_WT_EXTRA_COMPRESSORS=ON`, opt-in — the default
build and its dependencies are unchanged), and the block compressor swept on
the document and oplog tables. 8 writers, 1 GB cache, 8 KiB documents:

| payload | compressor | throughput | p99.9 latency | disk |
| --- | --- | ---: | ---: | ---: |
| incompressible | lz4 | **+86%** | **−97%** | 1.9× |
| incompressible | zstd | +73% | −93% | 1.67× |
| compressible | lz4 | **+15%** | **−88%** | 1.14× |
| compressible | zstd | +12% | −74% | 1.11× |

**lz4 nearly doubles write throughput and cuts p99.9 latency by 30× on
incompressible data**, and still wins clearly on compressible data where
zlib's ratio should be at its best. The cost is disk footprint.

This closes a long investigation: it is why the tail was CPU-bound rather than
I/O-bound, why a bigger cache helped (fewer evictions to compress), why no
eviction tuning helped, and a large part of why a real `mongod` measured 3.7×
faster with a 72× better tail in the three-droplet comparison.

It also refines an earlier conclusion rather than contradicting it. "Never turn
oplog compression off — throughput craters" remains true; uncompressed pages
mean more eviction I/O. But that compared zlib against *none*. The real axis is
*which* compressor, and zlib sits at the wrong point on the CPU/IO curve for
this engine.

**lz4 is now the default.** New document and oplog tables are created with it;
snappy and zstd measured close enough that they stay behind an opt-in build
flag rather than becoming two more link dependencies.

Existing data is safe. `block_compressor` is recorded per table at create time,
so a store written before this change has zlib tables — and zlib therefore
stays linked, deliberately and permanently. Verified end to end: a store
created by the previous release was opened by the new build, 201,982 documents
read back with zero errors, and subsequent writes added an lz4 table alongside
the existing zlib ones. A unit test records why the zlib extension must not be
removed as a tidy-up.

The trade is disk: expect roughly 1.9x the footprint on incompressible content
and 1.14x on compressible. A `--block-compressor` flag for deployments that
would rather have the space is tracked in `tasks/backlog.md`.

#### Changed

- **The default block compressor is now lz4** for the document and oplog
  tables. zlib remains built and linked so existing stores stay readable.

#### Added

- `SECANTUS_WT_EXTRA_COMPRESSORS` CMake option and matching build.rs env hook
  (with `SECANTUS_WT_EXTRA_LIBDIR`) to additionally build snappy and zstd.
- `SECANTUS_DATA_TABLE_EXTRA`, a create-time config hook for the document
  tables, mirroring the existing `SECANTUS_OPLOG_TABLE_EXTRA`.
