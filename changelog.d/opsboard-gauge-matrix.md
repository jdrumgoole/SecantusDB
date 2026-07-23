### Ops Board: a gauge matrix for all thirteen driver suites

The Ops Board gains a **Gauges** page: every one of the thirteen
driver-conformance suites — pymongo, pymongo-async, Go, Node, Java, Kotlin,
Ruby, Rust, PHP (library and extension), C, C++ and C#/.NET — listed with a Run
button for each server, so any single gauge can be pointed at either the Python
or the Rust server without dropping to the CLI. Each row shows the local
toolchain that gauge needs and its expected duration, and its info dialog
explains what that particular suite proves — why Go and the PHP extension are
the strictest wire-protocol checks, why the Node include set is deliberately
narrow, why the Ruby gauge stays lite-only, and which gauge binds port 27017.

The matrix is generated from a declared gauge catalog rather than hand-written
per combination, so adding a gauge is a one-line edit and the page can never
drift into claiming a capability that no longer exists.

#### Added

- `/gauges` page: 13 gauges × 2 servers, data-driven from `registry.GAUGES`,
  with per-gauge toolchain requirements, time estimates and info dialogs.
