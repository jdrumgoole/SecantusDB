# secantus-compressors

The snappy 1.1.10 and zstd 1.5.5 libraries that mongod 8.2 links, compiled
from mongo r8.2.11's own copies (`vendor/snappy`, `vendor/zstandard`) with
the source lists of mongo's `BUILD.bazel`. WiredTiger's builtin snappy and
zstd compressor extensions call them, so SecantusDB stores are compressed
exactly as mongod's are.

Linking this crate links the two static libraries. Its build script also
publishes their include directories to dependants (`DEP_SECANTUS_COMPRESSORS_SNAPPY_INCLUDE`,
`DEP_SECANTUS_COMPRESSORS_ZSTD_INCLUDE`), which `secantus-wiredtiger-sys` uses
to configure WiredTiger.

Internal to SecantusDB: no semver promise.
