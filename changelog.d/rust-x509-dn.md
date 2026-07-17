### Rust server: MONGODB-X509 authentication actually works now

The Rust server's TLS, mTLS, and MONGODB-X509 machinery was in place, but the
first end-to-end test showed the peer-certificate DN was extracted in
x509-parser's raw display form — least-specific-first with comma-space
separators — so the identity a client's certificate asserted never matched
the user record provisioned for it, and X509 authentication always failed.
The extraction now produces the mongod-style RFC 4514 string
(most-specific-first, bare commas, short OID names, value escaping),
byte-identical to the Python server's conversion, so a user provisioned
against either server authenticates on the other. A full two-stage
bootstrap-then-authenticate test now runs against the Rust server, mirroring
the Python suite.

#### Fixed

- Rust server: the MONGODB-X509 peer DN matches mongod's RFC 4514 form; X509
  auth verified end-to-end (TLS handshake, mTLS client-cert requirement, DN
  identity, and the no-client-cert refusal path).
- Rust server: tailable cursors on capped collections verified live and
  pinned by a smoke test (the shipped `find.rs` producer had outlived its
  "still deferred" note).
- `secantus-wt`'s build probe also accepts MSVC's `wiredtiger.lib` (and
  `.dylib`), unblocking the standalone `secantusd-rs` build on Windows.

#### Changed

- A 2026-07-17 backlog audit closed five stale Rust-rewrite entries: the
  `find` command entry (shipped long since), Phase-4 sub-phase 5e and the
  storage-keystone engine-selection half (superseded by the two-server
  model), the keystone wheel-flag flip (already ON in the shipping matrix),
  and the standalone-binary half of the Rust-package entry (`secantusd-rs`
  ships). The crates.io publish and the "recommend Rust by default" call are
  explicitly flagged as product decisions.
- R8: the mongo-go-driver gauge now runs against the Rust server — 398
  passed / 3 failed / 52 skipped (99.3%), unified suite 42/42; the single
  real failure is the documented, accepted go-harness `try_next` load-timing
  artifact. Report at `docs/validation-report-go-rust-server.md`.
