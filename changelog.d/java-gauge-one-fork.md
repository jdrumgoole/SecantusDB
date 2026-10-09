### The Java driver gauge no longer deletes its own test data

The Java gauge reported 495 of 496 for the Rust MongoDB server, with a
different test failing on each run, always a read that was missing documents
inserted a moment earlier. That reads like lost writes. It was the gauge.

The driver's test fixture drops the shared `JavaDriverTest` database when each
test JVM exits, and the gauge overrode the driver's own one-JVM setting to run
twelve at once against one server. A JVM that finished deleted the data under
the ones still running. A logging proxy in front of the server showed a
`dropDatabase` from another connection 2 ms before the failing read.

#### Fixed

- `invoke validate-java` runs one test JVM at a time by default, as the driver
  itself does. `SECANTUS_GAUGE_PARALLEL_FORKS` still raises it for timing
  experiments.
- The Rust server's Java panel is corrected to 496 of 496.
- `invoke validate-kotlin` does the same: its tests use the same fixture. It
  had not shown a failure, and one JVM at a time reports the same 340 of 340
  on both servers.
