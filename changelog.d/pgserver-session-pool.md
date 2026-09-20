### A WiredTiger session per statement, for statements that never read a row

Every statement the Rust PostgreSQL server runs outside a transaction block
opened a WiredTiger session and closed it again — measured at exactly one per
statement, including `SELECT 1`, which touches no data at all. Sessions are
cheap but not free, and a client such as psycopg sends every statement through
the path that pays for one.

A session whose transaction has finished holds nothing: the transaction is
over, and any cursor closed with the scope that opened it. So rather than
closing them, finished sessions are now parked and handed to the next
transaction that asks. The pool is bounded, because a session is a WiredTiger
resource the connection's own limit governs, and over the cap a session is
closed as before — the pool can never grow beyond what concurrent work actually
needed. A session whose commit failed is never returned: closing it is what
rolls its dead transaction back.

An extended-protocol `SELECT 1` now costs 51.4 microseconds where it cost 55.2.

#### Changed

- Finished user-transaction sessions are pooled and reused rather than closed,
  bounded, with failed commits excluded.
