### The Rust MongoDB server honours mongod's connection rate limiter

mongod 8.2's ingress connection-establishment rate limiter is honoured: its
four parameters can be read and set, and while it is enabled each new
connection takes a token from a bucket of `ratePerSec x burstCapacitySecs`,
waits its turn when the bucket is empty (no more than `maxQueueDepth`
waiting), and is closed unanswered past that, as mongod 8.2.11 does.

#### Added

- `setParameter` / `getParameter`: `ingressConnectionEstablishmentRateLimiterEnabled`,
  `ingressConnectionEstablishmentRatePerSec`,
  `ingressConnectionEstablishmentBurstCapacitySecs` and
  `ingressConnectionEstablishmentMaxQueueDepth`, with mongod's defaults,
  coercion and refusals; the bucket in `ServerParams::admit_connection`,
  asked by every accepted connection. Fixes RM-008's rate-limiter half
  (tests/backlog/test_rm_001_028.py::test_ingress_connection_rate_limiter_parameters,
  ::test_ingress_rate_limiter_admits_queues_and_closes). The Go gauge's
  `TestConnectionPoolBackpressure` now fails only for its `$where`, as on a
  `mongod --noscripting`, and is recorded as an expected failure.

#### Fixed

- `setParameter` with several parameters reports the previous value of the
  one whose name sorts first, as mongod does (it reported the first one
  written).
