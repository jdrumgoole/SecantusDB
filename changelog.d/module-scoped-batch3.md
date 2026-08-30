### Eight more test files share a server

The same treatment as the previous two rounds, applied to files that build their
server, client and database in a single fixture: one server per file instead of
one per test, with each test dropping what it created so the next sees a clean
server. Their combined runtime falls from 26 seconds to 3.5.

Five otherwise-eligible files were deliberately left alone because their fixture
inserts seed data. Sharing the fixture would run that seed once and the per-test
cleanup would then delete it out from under every later test — the tests would
still pass individually and fail together, which is the worst kind of change to
make casually.

#### Changed
- Eight further test modules share a module-scoped server.
