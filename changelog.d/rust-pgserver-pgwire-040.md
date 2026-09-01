### The Rust PostgreSQL server moves to a current wire-protocol library

The library that speaks the PostgreSQL wire protocol was pinned to a version
from nine releases ago. Two limitations that had been written down as costs of
using that library — errors that could not carry the name of the constraint they
violated, and no way to send data for `COPY ... TO STDOUT` — turned out to have
been fixed upstream months earlier. They were costs of the pin, not of the
library.

With the upgrade, both are closed. A duplicate key error now carries the same
constraint, table and schema names PostgreSQL sends, which is what the Java
driver reads when an application asks which constraint failed. `COPY table TO
STDOUT` produces output byte-identical to PostgreSQL's, so data copied out of
one server loads straight into the other.

The lesson is worth more than the features: a dependency pinned below 1.0 stops
receiving even compatible updates, and nothing announces that. Checking for a
newer release takes seconds and should happen before limitations get written
down as permanent.

#### Added

- `COPY <table> TO STDOUT` in text format, round-tripping with `COPY FROM`.
- Constraint, table and schema names on duplicate-key errors.

#### Changed

- The wire-protocol library moves from 0.31 to 0.40, clearing its deprecated
  calls at the same time.
