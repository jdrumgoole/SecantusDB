### CI: the Java-vs-Rust-server gauge joins the weekly validate run

`docs/validation-report-java-rust-server.md` was only refreshable by hand —
the weekly `validate.yml` run regenerated every other committed report but
not this one, so it would have gone stale. A `java-rust-server` matrix entry
now runs `invoke validate-java --server rust` weekly alongside the other
gauges: it reuses the java gauge's JVM/Gradle toolchain plus the
storage-engine sync, and points `gauge_common.rust_binary` at the
venv-staged `secantusd-rs` via `SECANTUSDB_BIN` (the default search only
covers the cargo target dir).
