### The concurrency benchmark measures all three servers

`bench.concurrency` grows a `--server python|rust|mongod|all` switch —
`all` sweeps the three back-to-back and prints a combined
throughput-vs-writers table — plus two diagnosis instruments the first
run immediately paid for: failing writers dump their log tails instead of
silently zeroing, and `--server-log` captures the server's own
stdout/stderr. [Concurrency](https://secantusdb.com/docs/concurrency.html)
now carries the measured end-to-end table (mongod scales to 4.1× at 8
writers; the Rust server holds flat behind its global write mutex; the
Python server degrades under the shared oplog-meta hotspot — with
conflicts retried and surfaced honestly since the commit-conflict fix this
harness uncovered).
