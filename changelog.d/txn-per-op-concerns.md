### Read and write concerns inside a transaction are refused, not ignored

A transaction's concerns are settled when it begins: its read concern rides
the statement that starts it, and its write concern belongs to the commit.
Attaching either to a statement in the middle is meaningless, and real
MongoDB says so with an `InvalidOptions` error. SecantusDB accepted them and
quietly did nothing, so a caller could believe a statement had run at a
durability or isolation level it never had.

Drivers already refuse this on the client side, which is why no driver test
suite ever caught it. It surfaces for anyone issuing raw commands — the one
audience with no other way to tell us apart from a real server.

#### Fixed

- A `writeConcern` on an in-transaction statement is rejected with
  `InvalidOptions` (72) on both servers, matching MongoDB's wording.
- A `readConcern` on a statement that continues (rather than starts) a
  transaction is likewise rejected. The starting statement may still carry
  one, since that is how a transaction's read concern is chosen.
