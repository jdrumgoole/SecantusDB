### A change stream survives a transient error, as it should

A change stream is meant to be durable across a hiccup: when the server hits a
transient problem mid-stream, the client is supposed to quietly reconnect and
carry on from where it left off. That never happened here, because the server
gave the client no way to tell a transient failure from a fatal one.

MongoDB marks the errors a change stream may recover from, and drivers act on
that marking alone — never on the error code by itself. The Rust server sent
neither the marking nor, in fact, the errors: the mechanism test suites use to
provoke a mid-stream failure was accepted and then ignored, so nothing ever
went wrong to recover from. Both halves are now in place, so a change stream
interrupted by a transient error resumes instead of surfacing the failure to
the application.

The distinction MongoDB draws is preserved: an error injected inside the
change-stream path is recoverable, while the same error code injected at the
command boundary is not, and a fatal error stays fatal.

#### Fixed

- A change stream resumes after a transient server error rather than failing.
