### The C++ driver gauge no longer restamps a stale conformance report

When the mongocxx gauge refuses to start — its tests hard-wire
`mongodb://localhost:27017`, so it declines to run if anything already holds
that port — the task nonetheless went on to regenerate
`docs/validation-report-cxx.md` from whatever JUnit the *previous* run had left
behind. The report came out carrying the old numbers under today's date, which
is the one thing a conformance report must never do: a run that did not happen
looked like a run that passed.

The report is now written only from results the current invocation actually
produced. The task clears the raw JUnit before starting and refuses to render a
report if none comes back, naming the likely cause. A gauge run whose tests
*fail* still produces a report, as it should — the test binary writes its JUnit
either way, so a missing file means the gauge never ran rather than that it ran
badly.

#### Fixed

- `invoke validate-cxx` no longer regenerates the C++ validation report when
  the gauge bails before running (port 27017 in use, missing toolchain, failed
  build); it exits non-zero and leaves the previous report untouched.
