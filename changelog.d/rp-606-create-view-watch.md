### A CREATE VIEW that went unanswered for 10 seconds is filed as a watch item

A PostgreSQL regression test about view columns timed out once in CI on 2026-11-08: the server sent nothing for 10 seconds after a `CREATE VIEW`. The only change in that snapshot was in the MongoDB server's command layer, which the PostgreSQL server does not link. The case is filed as RP-606, with what to capture if it happens again.

#### Changed

- Filed RP-606, a watch item in the PostgreSQL backlog.
