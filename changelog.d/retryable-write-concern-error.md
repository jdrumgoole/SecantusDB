### A retryable write's retry no longer replays the original attempt's error

A `writeConcernError` describes one attempt: the write itself succeeded and only
its durability acknowledgement failed. The retryable-write replay cache stored
the reply verbatim — deliberately, so a replay would be byte-identical — which
meant a driver's retry was handed the very error it retried because of. The
operation then surfaced as an error even though the write was safely applied.

Separately, the failpoint-configured `writeConcernError` was being embellished
with a synthesised `errmsg` and `codeName`. Real mongod echoes the failpoint's
document verbatim, and the synthesised `codeName` was wrong anyway: it rendered
91 as `Location91` where 91 is `ShutdownInProgress`.

Together these were the last genuine failure in the `mongo-c-driver` gauge,
`/command_monitoring/unified/writeConcernError`, which now passes. The remaining
C-gauge failures are the documented inherent ones (IPv6 listener, and server
selection asserting a standalone/secondary that a single-node surrogate has not
got).

#### Fixed

- The retryable-write replay drops `writeConcernError` and `errorLabels`, so a
  retry reports the write's actual outcome. Byte-identical replay is right for
  the write result and wrong for the attempt's own condition. Fixed on both the
  Python and Rust servers.
- A failpoint's `writeConcernError` is echoed exactly as configured — `{code: 91}`
  stays `{code: 91}` — matching mongod. The Rust server was already correct here.
- A non-retryable write (no `lsid`/`txnNumber`) still reports its
  `writeConcernError` as before; only the replay is stripped.
