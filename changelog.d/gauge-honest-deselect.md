### A test was excluded for a reason that turned out not to be true

Two psycopg tests that connect to reserved, unroutable addresses failed twice
during full gauge runs, and were excluded with a note explaining that the host's
network stack had answered the address immediately instead of letting the
attempt time out. That explanation was plausible, was written as though it were
a measurement, and was never checked.

It does not hold. Connecting to those addresses in a loop fails six times out of
six after four seconds with exactly the error the test expects, and re-enabling
both tests in a full gauge run — with the machine simultaneously busy running
another test suite — passed them. The exclusion has been removed.

What remains true is that the two failures happened, took forty milliseconds
rather than four seconds, and are unexplained. That is now an open question with
a note on how to answer it: capture the exception text, because the errno
distinguishes a routing answer from resource exhaustion, and resource exhaustion
would make it a defect in this project's own test harness rather than a property
of the machine it ran on.

#### Fixed

- `test_connect_error_multi_hosts_each_message_preserved` and its async twin run
  in the psycopg gauge again.

#### Changed

- The unexplained failures are recorded as an open backlog item rather than an
  exclusion, with instructions to capture the error before forming a theory.
