### Benchmark figures refreshed after the RecordId change

The published per-operation numbers were measured before the Python server's
document table was re-keyed by RecordId, so they understated it. This is a fresh
three-way run — real `mongod`, the Rust server, the Python server — taken in one
sitting on one idle machine, which is the only way the ratios between them mean
anything.

The Python server's unsorted scan went from 12.0× mongod to 7.7×, `$group` from
22.4× to 17.2×, and insert from 4.9× to 4.3×. Its overall range is now about
4×–17× of mongod rather than 5×–23×, and the Rust server is about 4×–10× faster
than it rather than 4×–13×. Two workloads moved the other way — an indexed range
read (6.5× → 7.0×) and `update_many` (12.1× → 12.7×) — because both now unpack
the document's key out of the stored row. The mongod and Rust-server timings
reproduced the previous run to within a percent or two, which is what makes the
Python-side movement readable as a real change rather than machine noise.

#### Changed

- `docs/benchmark.md`, the README and the Rust server's docs index carry the
  new figures; the latency chart is regenerated from them.
