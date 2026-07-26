### A driver gauge that cannot run no longer leaves a report saying it passed

Each driver-conformance gauge writes a raw results artifact — JUnit XML, a
`.trx`, newline-JSON — which a second step renders into a validation report.
The runners are invoked tolerantly on purpose, because a gauge whose tests
*fail* still owes you a report; that is the deliverable. But the same tolerance
let a gauge that never ran at all fall through to the rendering step, which
happily re-rendered the *previous* run's artifact under today's date. A run that
did not happen came out looking like a run that passed.

This was found on the C++ gauge, which refuses to start when something already
holds port 27017 — its tests hard-wire the driver default and cannot be
redirected — and fixed there first. Every other gauge shared the shape: only
Java and Kotlin guarded themselves, and they did it by inspecting the runner's
exit code, which is the wrong signal for gauges whose runner returns non-zero
when tests legitimately fail.

All thirteen gauges now go through one helper that clears the artifact before
the run and refuses to render a report unless fresh results come back, with a
gauge-specific hint about the likely cause. Keying on the artifact rather than
the exit code is what lets a failing run still report while a run that never
started does not.

#### Fixed

- No gauge task regenerates its validation report from a previous run's
  results. Applies to go, node, ruby, rust, java, kotlin, php-lib, php-ext, c,
  cxx, dotnet, psycopg and slt.

#### Changed

- Java and Kotlin move from an exit-code guard to the shared artifact guard, so
  a legitimately failing run reports instead of being suppressed.
