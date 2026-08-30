### The seeded test files share a server too, without sharing their data

Five test files were held back from the previous rounds because their fixture
inserts seed data before each test. Sharing that fixture would have seeded once
and then let one test's writes reach the next.

They now split the difference: the server is created once per file, but each test
still gets its own client, its own fresh seed, and drops its database on the way
out. Only the cost is shared, never the state. Their combined runtime falls from
40 seconds to 3.

#### Changed
- Five further test modules share a module-scoped server while keeping their
  per-test seeding and isolation.
