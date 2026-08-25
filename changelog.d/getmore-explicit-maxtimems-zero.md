### An explicit `maxTimeMS: 0` no longer blocks

`getMore` treated an explicit `maxTimeMS: 0` the same as an absent one, because
`doc.get("maxTimeMS", 0)` yields `0` for both — so a poll that asked for no
waiting got the one-second default anyway.

#### Fixed

- mongod distinguishes the two: an explicit zero is a non-blocking poll, an
  absent field means wait. Drivers rely on it, and blocking there does not
  merely slow the call down — it changes the answer, because an event occurring
  during the wait comes back to a client that asked what was ready *now*. Found
  via mongo-go-driver's change-stream suite, where a collection dropped by test
  teardown surfaced as a `drop` event the client had just been told did not
  exist. The Rust server already behaved correctly, so this also closes a
  two-server parity gap.
