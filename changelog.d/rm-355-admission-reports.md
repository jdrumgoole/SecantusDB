### The connection rate limiter and `$currentOp` report what admission control did

mongod rate-limits new connections when `ingressConnectionEstablishmentRateLimiterEnabled` is on. A connection takes a token from a bucket that holds `ratePerSec x burstCapacitySecs` tokens and fills at `ratePerSec`. With no token free, it borrows one still to come and sleeps until that token is due. It is rejected when more than `maxQueueDepth` connections are already asleep. The Rust MongoDB server had its own approximation of this bucket and no statistics for it. It now runs a port of mongod 8.2.11's `RateLimiter` and `SessionEstablishmentRateLimiter`, and reports them where mongod does. A `$currentOp` row also now says what admission control has done for its operation.

#### Added

- `serverStatus.queues.ingressSessionEstablishment`: the limiter's queue, admission and rejection counts, `averageTimeQueuedMicros` once a connection has been admitted, and `totalAvailableTokens` capped at INT64_MAX. Each field has mongod 8.2.11's type. It is reported whether the limiter is on or off, as on mongod.
- `serverStatus.connections.queuedForEstablishment` and `establishmentRateLimit` (`rejected`, `exempted`, `interruptedDueToClientDisconnect`).
- A `$currentOp` row's `queues` and `currentQueue`. `queues` gives the ingress and execution admissions for its operation, the time queued (a wait under way included) and whether it holds a ticket now. `currentQueue` names the pool the operation is waiting in now and how long it has waited, or is null.

#### Changed

- The limiter now reads its bucket the way mongod's does. The bucket is full when the server starts. A new rate or burst keeps the tokens the bucket holds. A burst below one token admits every connection at once. A connection in the queue checks once a second whether its client has gone; if it has, the connection leaves the queue, returns its token and is counted as interrupted.

#### Fixed

- Fixes RM-355 in part (tests/backlog/test_rm_355_364.py::test_admission_control_is_reported_where_mongod_reports_it), with the values compared against mongod in tests/mongod_live/test_ingress_admission.py::test_a_current_op_row_says_what_admission_control_admitted and tests/mongod_live/test_ingress_admission.py::test_the_connection_rate_limiter_is_reported. What remains of RM-355 is the ticket points, pinned by tests/backlog/test_rm_355_364.py::test_ending_a_transaction_takes_mongods_execution_tickets. RM-550 is filed for the limiter's bypass list (`ingressConnectionEstablishmentRateLimiterBypass`), which this server does not register.
