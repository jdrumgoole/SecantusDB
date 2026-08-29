### A partial index reports its `WHERE` clause

`pg_indexes.indexdef` and `pg_get_indexdef()` rendered a partial index as though
it covered the whole table — the predicate was simply missing from the generated
`CREATE INDEX` statement. Anything that recreates an index from that string,
which is the usual reason to read it, built a full index instead of a partial
one.

The predicate is now reconstructed, matching PostgreSQL's own rendering exactly
across the comparison operators, `IS NOT NULL`, `AND`/`OR` combinations, and
string literals with their `::text` cast. `WHERE b <> 5` round-trips too, which
takes a little care: it is stored internally as "not equal *and* not null", and
PostgreSQL reports the predicate you wrote.

A predicate shape that cannot be reproduced exactly still renders without the
`WHERE`, as before. That is deliberate — an approximate predicate in a statement
meant for recreating an index would build the wrong index quietly, which is
worse than an obviously incomplete one.
