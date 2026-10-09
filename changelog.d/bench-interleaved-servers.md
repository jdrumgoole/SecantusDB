### The per-operation benchmark measures its servers interleaved

Three droplet runs of one build put the Rust server's multi-stage aggregation
at 2.8×, 3.6× and 4.1× of `mongod`. The harness measured each server's reps
back to back, so minutes separated the two halves of every ratio and a noisy
neighbour on the shared-CPU droplet moved one column only.

#### Changed

- `bench.compare_servers` runs every server inside each rep, rotating the
  order, and `invoke do-perf` takes fifteen reps instead of five. Two droplet
  runs of the same build that way agreed on multi-stage aggregation to 0.24×
  (was 1.24×) and on `delete_many` to 0.02× (was 1.02×).
- The results file records, per workload, the lowest and highest ratio among
  the individual passes (`rust_x_range`, `py_x_range`), so a noisy run shows
  as one.
- The published per-operation figures now come from that harness. Across
  three interleaved droplet runs the Rust server is at 1.0×–3.2× of `mongod`
  (published before as 1.0×–4.1×), the Python server at 2.0×–26.7×, and the
  Rust server 1.9×–13.5× faster than the Python one. A third run widened the
  agreement on multi-stage aggregation to 0.63×: better than 1.24×, and
  still the row that moves most.
