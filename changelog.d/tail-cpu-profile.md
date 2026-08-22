### The p99.9 tail is CPU-bound, and two thirds of that CPU is zlib

Profiling the three-droplet cluster on Linux settled what the tail
investigation had left open.

Sampling every server thread's scheduler state every 20 ms and intersecting it
with the clients' recorded stall windows shows that **during a stall the threads
are running, not blocked**: 75.8% in state R inside stalls against 2.1%
outside, with uninterruptible disk I/O at 0.9% and lock waiting actually
falling. That retires the earlier "blocked on disk reads" reading.

A CPU profile then names the work. By shared object, **65.5% of the server's
CPU is inside `libz` — zlib block compression** — with every top symbol being
`deflate` under WiredTiger's page reconciliation.

That single fact ties the whole investigation together: application threads
conscripted into eviction spend their time compressing, which is why a bigger
cache helps, why smaller documents help more, why eviction thread count does
not help on a 4-vCPU box, and why bounding concurrency did not help either.

It also points at a lever nobody has pulled. Earlier work established that
turning oplog compression *off* craters throughput, and that is true — but it
compared zlib against none. The real choice is zlib against a *cheaper*
compressor: MongoDB defaults to snappy, roughly an order of magnitude cheaper
to compress at a worse ratio. SecantusDB pays zlib CPU on every page
reconciliation. The build currently enables only the zlib WiredTiger
extension, so selecting an alternative is a build-flag change before it is a
tuning question; the work is scoped in `tasks/backlog.md`.
