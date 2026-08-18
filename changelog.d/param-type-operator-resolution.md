### A parameter's declared type is resolved at Parse, like Postgres

Postgres picks the operator for a comparison while it is still analysing the
statement, before a single row is read. That means a prepared statement
comparing a `varchar` column against a parameter declared as `uuid` is rejected
outright — `operator does not exist: character varying = uuid` — rather than
quietly becoming a predicate that never matches. SecantusDB already restored
that behaviour for literals, but a parameter was treated as undecidable, so the
same query returned zero rows and no error. A client that declares its parameter
types (psycopg does, from the Python object it dumps) could silently get an
empty result for a query Postgres would have refused to prepare.

The declared parameter types are only known at Parse, so the check now runs
there too, with the OIDs the client sent. The error message names the *declared*
type rather than the internal storage tag: a `varchar` column folds to `text`
internally, and reporting `text = uuid` would have been a message no real
Postgres ever emits. Both the SQLSTATE and the wording were probed against a
real PostgreSQL 14.

The analysis stays deliberately sound-but-incomplete — an undeclared parameter
(Postgres' `unknown`, which takes the other operand's type) and any type pair the
categories don't model are left alone, because a spurious error breaks a working
query, which is far worse than the lenient old behaviour.

#### Fixed

- A comparison against a parameter whose declared type has no operator against
  the other operand now raises `42883` at Parse, instead of silently matching
  no rows. The message names the declared type (`character varying`, not
  `text`).
