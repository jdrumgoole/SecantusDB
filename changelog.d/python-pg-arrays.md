### Four wrong answers the Python PostgreSQL server gave about arrays

The Rust PostgreSQL server's array work last release left a corpus that could be
pointed at the Python server too. It was not flattering: 18 divergences out of
32 against PostgreSQL 14.13, where the Rust server had none. Four of them were
*wrong answers* rather than refusals, and one of those was wrong in two
independent places.

`ARRAY[1,NULL] @> ARRAY[NULL]` answered true. So did `&&`, and
`ARRAY[1,NULL] <@ ARRAY[1,NULL]`. PostgreSQL says false to all three: the
containment operators use the element type's `=`, under which a NULL matches
nothing — not even another NULL. Python's `None == None` is True, and that
stood in for the SQL semantic. In a `WHERE` clause it returned rows PostgreSQL
excludes.

The same predicate had a second copy of the bug in the index path, which had to
be fixed separately: `field @> ARRAY[...]` lowers to a Mongo bare-equality
filter so it can use a multikey index, and a bare equality against null matches
a *missing* field as well. Fixing the evaluator alone would have left the
indexed form of the same query still wrong.

Separately, `array_position(arr, elem, start)` ignored its third argument
entirely, so `array_position(ARRAY[1,2,3,2], 2, 3)` answered 2 where PostgreSQL
answers 4 — a wrong number, which is why nothing had caught it. sqlglot files
that argument under a slot named `zero_based`, which it reuses; the obvious
place to look for it holds nothing at all.

And a multidimensional subscript — `m[1]`, `m[1:2][2]` — leaked a bare Python
`ValueError` to the wire with no SQLSTATE at all, reachable from a plain
`SELECT`.

#### Fixed

- `@>` / `<@` / `&&` no longer match a NULL to a NULL, in both the per-row
  evaluator and the index pushdown.
- `@>` / `<@` / `&&` now flatten both operands, so
  `ARRAY[[1,2],[3,4]] @> ARRAY[3]` is true as PostgreSQL has it; previously
  containment was wrong for every multidimensional array.
- A NULL array operand to any of the three is now NULL rather than
  `42883 function array_contains_all() does not exist` — an internal fallthrough
  re-labelled as a missing function the user never called.
- `array_position(arr, elem, start)` honours `start`, and a NULL `start` is
  PostgreSQL's `22004 initial position must not be null`.
- `array_cat(NULL, NULL)` is NULL, not the empty array.
- `array_length(arr, NULL)` is NULL rather than a `42883` naming
  `array_size(...)`.
- `string_to_array(s, '')` keeps the whole string as one element rather than
  raising, and `string_to_array('', sep)` is the empty array rather than `{''}`.
- `array_positions` is declared `integer[]`, which is what PostgreSQL reports,
  not `bigint[]`.
- A subscript chain shorter than the array's dimensionality is NULL —
  `(ARRAY[[1,2],[3,4]])[1]`, which used to return the inner row and then fail
  to coerce it, leaking a Python `ValueError` with no SQLSTATE.
- Once any subscript in a chain is a slice, a bare index means "1 to n" as
  PostgreSQL defines it, so `m[1:2][2]` is the whole second dimension. The
  result type is now taken over the whole chain, so a nested slice reports the
  array type rather than the element type.

#### Added

- `UPDATE t SET a[i] = v` and `SET a[lo:hi] = v`, matching the Rust server:
  a subscript past the end extends the array with NULLs, assigning into a NULL
  column builds it from nothing, and two assignments to one column in one
  statement compose rather than racing.
- A subscript below 1 is refused by name. PostgreSQL answers it by *moving* the
  array's lower bound, which neither server models; writing it at 1 instead
  would silently shift every other subscript into the array.

#### Security

- `SET a[1000000000] = 1` is one statement and would have allocated every slot
  it named. PostgreSQL caps an array at 134217727 elements and says so; the cap
  is now checked before allocation.
