### The pymongo gauges now separate "unsupported" from "broken"

Both pymongo gauges — the sync suite and the `AsyncMongoClient` one — had
a handful of red tests that were never going to go green, because every
one of them exercises something SecantusDB deliberately does not
implement: hashed indexes, text indexes, and `$where`, which needs the
embedded JavaScript runtime mongod ships and SecantusDB does not. The
server already answers each with a faithful "not supported" error; the
tests fail because they asked, not because anything is wrong.

Two of them are worth naming precisely, because their titles suggest
otherwise. `test_maxtime_ms_message` and `test_to_list_csot_applied` are
about timeouts, not about `$where` — they merely use `$where` to make a
query slow enough to time out. Since the query is rejected up front, they
never reach the behaviour they are named for. They are recorded as
unverified rather than as passing: the gauge tells us nothing about
maxTimeMS message shape or CSOT either way.

#### Changed

- The six pymongo / pymongo-async failures are now classified as expected,
  each with its rationale, so the summary counts them separately from
  failures that need a fix. Both gauges report zero actionable failures.
- The async gauge is wired to the shared expected-failures list; it runs
  the same upstream tests and hit the same gaps under different node IDs.
