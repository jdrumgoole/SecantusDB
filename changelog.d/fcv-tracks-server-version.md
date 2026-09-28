### `featureCompatibilityVersion` said 7.0 while the server said 8.2.11

`getParameter` reported `featureCompatibilityVersion: {version: "7.0"}` on both
servers. The handshake right next to it reported `buildInfo.version` `8.2.11`,
`versionArray` `[8, 2, 11, 0]` and `maxWireVersion` 27 — so the value
contradicted the same server's own answers, not merely mongod's. A client
gating a feature on FCV was told this is a 7.0 deployment.

Measured against a mongod 8.2.11, which answers `{"version": "8.2"}` — the
major.minor of the running binary.

#### Fixed

- Both servers now derive the string from `SERVER_VERSION_ARRAY` instead of
  carrying a second copy of the version. As a literal it survived the retarget
  from 6.0 to 8.x untouched while every other version surface moved; deriving it
  is what stops that recurring.

#### Added

- A differential test asserting FCV equals mongod's, and a value assertion in
  the Rust `get_parameter_named_and_all` unit test.

  The two tests that already looked at this parameter asserted only that the KEY
  was present — which is exactly the assertion a stale literal survives, and why
  nothing caught a wrong value for the whole life of the 8.x retarget.
