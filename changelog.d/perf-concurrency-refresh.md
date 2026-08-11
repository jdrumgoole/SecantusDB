### Fresh performance and concurrency numbers — and honest harnesses

Both benchmark reports are re-measured on current code (post
TCP_NODELAY, batched sequences, and the parse cache). Per-operation
latency: the Rust server runs at **0.7×–2.2× of mongod**, with three
workloads now beating mongod outright (change-stream drain 0.7×,
delete and single-stage `$group` 0.9×); the Python server spans
1.2×–24×. Write scaling is unchanged in shape and confirmed healthy:
the Rust server scales monotonically to **2.5× at eight writers
(~93k docs/s fully durable)**, the async oplog stack reaches ~107k.

Getting trustworthy numbers surfaced two real defects. The concurrency
harness handed writer 0 a `drop` that raced the other writers' insert
stream — a drop starved behind continuous batches for the whole window,
died summary-less on SIGTERM, and rows silently averaged a dead writer,
manufacturing a 3.4× phantom regression. Writers now target fresh
per-row collections (no drops near the measurement), install signal
handlers before any I/O, and a missing writer summary fails the run
instead of shipping a corrupt row. Second, dropping a heavily-churned
collection can wedge the Rust server behind a WiredTiger eviction storm
that survives client disconnect and SIGTERM — captured with native
stacks and filed in `tasks/backlog.md` for its own slice.

#### Added

- `bench/latency_chart.py` + `bench/results/latency.json`: the latency
  chart, markdown table, and site table are now regenerated
  mechanically from one results file (they were hand-edited SVGs).
