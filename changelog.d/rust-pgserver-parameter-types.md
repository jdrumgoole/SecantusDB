### The type a parameter was sent as

A bound parameter carries a type the client declared, and this server was
throwing it away — reading each parameter's meaning back out of its decoded
value instead. Most of the time the two agree. Where they don't, the answers
were wrong in ways that print correctly.

`pg_typeof(%s)` with a small integer said `integer`, because that is what the
value looks like; PostgreSQL says `smallint`, because that is what psycopg
declared. And a parameter with no declared type at all has no type to report:
PostgreSQL answers an error rather than guessing, where this server guessed
`text`. The declared types now reach the planner, on the describe path as well
as the execute one — the describe runs first, so a describe that did not know
them answered for the whole statement.

Ranges had a sharper version of the same problem. A range over a discrete
element type has one true spelling — PostgreSQL rewrites every bound, so
`[10,20]` is stored and printed as `[10,21)` — and a range parameter kept
whatever the client wrote. `int4range(10, 20, '[]') = %s` with that very range
bound was **false**, while both sides printed identically. Two routes needed
fixing: a range parameter that arrives with its type now decodes through the
same cast a literal takes, and one that arrives untyped takes its type from the
operand beside it, which is what PostgreSQL does at analysis time.

#### Added

- Declared parameter types reach the planner, so `pg_typeof` reports them.
- `42P18` for a parameter whose type neither the client nor the context gives.

#### Fixed

- A range or multirange bound as a parameter compared unequal to the same range
  written any other way.
