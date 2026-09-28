### The Rust server ignored `maxTimeMS`

It was parsed and validated exactly as mongod validates it — every type error,
every bound — and then never checked. The operation ran to completion and
answered `ok`. Measured against mongod 8.2.11: a `createIndexes` over 100,000
documents with `maxTimeMS: 1` returned `ok: 1.0` here and `code 50
MaxTimeMSExpired` there.

That is invisible on a fast operation, which is why validating the argument so
carefully never exposed it.

#### Added

- `secantus_core::deadline` — a thread-local budget armed by `run_handler`
  around the whole handler, because that is the span mongod bounds: the
  operation, not any one loop inside it. Mirrors `src/secantus/deadline.py`
  deliberately, so the two servers overrun in the same places for the same
  reasons.

- Polling in the loops whose length is driven by the data: the document scan,
  the two predicate passes behind `find` and `count`, and the three index-build
  loops. Every 64 documents, which bounds the overrun to 64 rows rather than the
  whole table.

  `getMore` is excluded, as on the Python server — there `maxTimeMS` is the
  awaitData *wait*, not a limit, and arming a deadline would make every tailable
  poll report a timeout.

#### Measured

- Against mongod 8.2.11: **0 divergent of 4** — `createIndexes`, `find` + sort
  and `count` all answer code 50 where they previously answered `ok`, and an
  operation with no budget is untouched.
- **Cost on the unarmed fast path: none detectable.** A 100,000-document
  COLLSCAN with no `maxTimeMS`: 34.9 ms before, 34.7 ms after (−0.7% on the min
  of seven). The plan warned these polls sit in the flagship's hottest loops and
  said not to assume the cost, so it was measured rather than asserted.
