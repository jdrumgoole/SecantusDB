Title: secantus-pg 0.1.0-beta.5 no longer drops a cancel that arrives early
Date: 2026-10-09 18:00:00
Slug: secantus-pg-0-1-0-beta-5
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.5 keeps a CancelRequest that arrives while a statement is still being parsed, bound or described. Earlier releases dropped it and ran the statement to the end.

A query timeout in a PostgreSQL client is usually a cancel: the client opens a
second connection and sends a `CancelRequest` naming the first. Up to
0.1.0-beta.4, `secantusd-pg` lost that request if it arrived too soon. A cancel
that landed while the statement was still being parsed, bound or described was
dropped, the statement ran to completion, and the client waited for all of it.

The server cleared its pending-cancel flag at the moment a statement began
executing, on the reasoning that a cancel received while idle has nothing to
cancel. That holds for an idle backend. It does not hold for one that has
already started on the client's message.

We measured it against PostgreSQL 15.19 with a statement that takes seconds to
parse. PostgreSQL answered `57014 query_canceled` for a cancel sent anywhere
from 0 to 640 ms after the statement. This server ran the full sleep at every
one of those offsets. That is also how one test in psycopg's own suite,
`test_generators.py::test_cancel`, came to run its `pg_sleep(180)` to the end
on our Windows runner.

In beta.5 a cancel is dropped only if it arrives before the first message of a
request, when the backend really is idle. One that arrives later stays pending
until the statement reaches a point where it can stop. Two smaller faults went
with it:

- A statement nested inside another, such as the statements in a `DO` block,
  no longer clears a cancel aimed at the block.
- A cancel is used up by the statement it interrupts, so a handler that
  catches `query_canceled` is not cancelled a second time by the same request.

Two differences from PostgreSQL remain. The cancel takes effect when the
statement starts executing, not during the parse: against a statement that
took 2.8 seconds to parse here, PostgreSQL answered in 30 ms and this server
answered once the parse had finished. And a cancel that lands while the server
is still reading a very large statement off the socket is still dropped; we
saw that in the first 5 ms after a 1.4 MB statement, in a debug build.

This is a server for tests. It is single-node, it is a beta, and a role with
no password connects without one, so keep it on loopback.

`cargo install secantus-pg --version 0.1.0-beta.5` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.5)
