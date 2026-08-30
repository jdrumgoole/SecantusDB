### Nine more test files share a server, by cleaning up after themselves

A previous change let test files share one database server, but only where every
test happened to use a differently-named collection. That ruled out most files —
not because they genuinely needed their own server, but because two tests
happened to pick the same collection name.

Those files now share a server too, and get their clean slate a cheaper way: each
test drops the databases it created when it finishes, so the next test sees a
server that looks new. That turns out to cost far less than starting a fresh
server, and it needed no changes to any test's body. The nine files run in 6
seconds instead of 23.

The cleanup runs after the test rather than before, so a failing test leaves its
data in place to be inspected.

#### Changed
- Nine further test modules share a module-scoped server, with a per-test
  database reset providing isolation.
