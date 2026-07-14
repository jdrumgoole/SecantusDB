### Docs: the Rust-server Java gauge report joins the site

`invoke validate-java --server rust` has been writing
`docs/validation-report-java-rust-server.md` — the mongo-java-driver suite
pointed at the standalone Rust server — but the report had never been
committed or added to the docs toctree. It now ships alongside the other
validation reports (445/2 passed, 99.6%; the two failures are the
`mapReduce` tests, consistent with the Rust server not implementing
`mapReduce`).

#### Fixed

- `java_validation/generate_report.py`: the generator emitted the
  Python-server title and refresh command for both servers; a
  `-rust-server` output now gets a `(Rust server)` title, the
  `--server rust` refresh command, and a note that the two-phase spawn
  boots `secantusd-rs`.
