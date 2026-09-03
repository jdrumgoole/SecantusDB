### Range types, and the rewrite that makes two of them the same range

A range like `[1,5)` has bounds that may each be inclusive or exclusive, or
absent entirely. Over a type whose values are *discrete* — integers, dates —
that leaves several ways to write the same thing, so PostgreSQL picks one:
every bound is rewritten to `[)`. `[1,5]` becomes `[1,6)`, `(1,5)` becomes
`[2,5)`, and the two compare equal because they are, in fact, the same range.

Over a *continuous* type there is no such rewrite, because there is no next
number to move a bound to: `[1.0,2.0]::numrange` stays inclusive at both ends.
The Rust PostgreSQL server now supports both families — `int4range`,
`int8range` and `daterange` on the discrete side, `numrange`, `tsrange` and
`tstzrange` on the other — as literals, constructors and cast targets, each with
its own type oid so a client builds a range object rather than reading text.

Some details that only a real server tells you. An absent bound prints as
nothing at all, so an unbounded range is `(,5)` rather than anything spelled
with infinity. A range whose bounds meet without including each other contains
nothing and *is* the empty range, so `int4range(1,1)` prints as `empty`. And a
bound gets quoted when its own text would be ambiguous between the brackets,
which a timestamp always is, because it has a space in the middle.

Three different mistakes get three different error classes, which is worth
keeping distinct even though all three refuse the query: a crossed bound is a
data error, a malformed literal is an invalid-text one, and unrecognised bound
flags are a syntax error.

#### Added

- `int4range`, `int8range`, `daterange`, `numrange`, `tsrange` and `tstzrange`
  as literals, constructors and cast targets, with canonicalisation, empty
  ranges, unbounded ends, bound quoting and their own type oids.
