### Several commands in one query, and a flake that was a missing feature

PostgreSQL's simple query protocol takes any number of commands separated by
semicolons and answers with one result each — `create table ...; insert ...;
select ...` in a single round trip. The Rust PostgreSQL server refused the whole
string, which is why so much client code failed before reaching the query it
cared about: the setup was the batch.

The part that cannot be added later is the transaction. A multi-command batch
runs as one implicit transaction, so a failure in the third command discards
what the first two wrote — and an explicit `COMMIT` inside the batch ends that
transaction, so whatever it committed survives a later failure. Both rules were
measured against a live PostgreSQL rather than assumed, and both fall out of
reusing the session's own transaction slot instead of tracking a second one.

`DEALLOCATE ALL` is now accepted too, and it is worth saying why it mattered.
Clients issue it to reset their prepared-statement cache, but only when the
connection happens to have one — so refusing it failed a scattered handful of
tests that varied from run to run. That looked exactly like flakiness, and it
was a missing feature the whole time.

`pg_typeof(x)` answers the type's display name — `integer` rather than `int4`,
`timestamp without time zone` rather than `timestamp` — reported as a `regtype`.
It reports the type the server would give the expression, not one read off the
value, which is why `pg_typeof(NULL)` is `unknown`.

#### Added

- Multi-command simple queries, one result per command, run as a single
  implicit transaction with PostgreSQL's commit and rollback behaviour.
- `DEALLOCATE ALL`.
- `pg_typeof()`, and the type display names behind it.

#### Fixed

- Casting a decimal to a float failed outright (`1.5::float8` raised "invalid
  input syntax"), and casting one to an integer failed the same way. Decimal
  literals became exact numerics in the previous release and these two cast
  paths were never taught about them.
- `2.5::float8::int` answered 3. PostgreSQL rounds float-to-integer half to
  even and numeric-to-integer half away from zero; one rule was being used for
  both. A large numeric is now rounded on its digits rather than through a
  float, which cannot represent every value a numeric can hold.

#### Changed

- Several commands in one *prepared* statement now raise PostgreSQL's own
  `42601` "cannot insert multiple commands into a prepared statement" instead
  of reporting the batch as an unsupported feature. The extended protocol has
  one parameter list and one row description, so this is a real error rather
  than a gap.
- An unsupported function now names itself (`function chr() is not supported
  yet`), where every one of them used to report the same `FuncCall`.
