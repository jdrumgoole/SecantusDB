### A conformance run that was cut short no longer reads as a clean sweep

The pgjdbc gauge wiped its results directory at startup and only aggregated
them once Gradle returned, so a run that hit the wall-clock budget reported
zero tests — which looks identical to a flawless run at a glance, and was read
that way once. A truncated run now keeps whatever did complete, records that it
was cut short, and exits with the conventional timeout status.

The report generator refuses to render a truncated run at all. That is the
important half: a partial run's per-class numbers are every bit as correct as a
complete one's, and only the *set of classes* is short — so publishing it
produces a healthy-looking pass rate quietly measured over less of the suite.
There is no caveat that reliably survives being pasted into a summary, so the
artifact simply isn't produced.

The budget itself is raised and made overridable via `SECANTUS_PGJDBC_TIMEOUT`,
because CI hardware runs several times slower than a development machine and
the suite legitimately grew once the crashes that used to end tests in
milliseconds were fixed.

#### Added

- `SECANTUS_PGJDBC_TIMEOUT` overrides the gauge's Gradle budget (default two
  hours, up from one).

#### Fixed

- A timed-out pgjdbc gauge aggregates partial results and reports the run as
  truncated instead of silently summarising zero tests.
- `generate_report` refuses to publish a conformance rate computed from a
  truncated run.
