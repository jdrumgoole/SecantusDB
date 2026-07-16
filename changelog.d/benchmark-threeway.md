### Benchmark: the Rust server measured — 2.1×–4.5× of mongod

`docs/benchmark.md` is regenerated as a three-way comparison
(`bench.compare_servers`): real `mongod`, the Rust server, and the Python
server, six workloads end-to-end through `pymongo` on on-disk WiredTiger.
The Rust server lands at **2.1×–4.5× of mongod** per operation and
~2.7×–5.2× faster than the Python server workload-for-workload; the Python
server sits at 6×–20.5× of mongod on this run. The Rust docs tree cites the
numbers, and its releases page now links the `secantusdb-v`-filtered
GitHub listing (binary releases are pre-releases, so the bare releases page
leads with the source-only PyPI release).
